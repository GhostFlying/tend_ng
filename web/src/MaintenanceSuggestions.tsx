import { useEffect, useState } from "react";
import { listDiscussions, notifyDiscussionChanged } from "./discussionApi";
import type { Discussion } from "./discussionApi";
import {
  discussMaintenance,
  handoffMaintenance,
  ignoreMaintenance,
  listMaintenance,
  MaintenanceRejection,
  readMaintenance,
} from "./maintenanceApi";
import type {
  MaintenanceDetail,
  MaintenancePage,
  MaintenanceView,
  ShareReceipt,
  Suggestion,
} from "./maintenanceApi";
import { Markdown } from "./Markdown";
import { replacePageQuery } from "./navigation";
import "./maintenance-suggestions.css";

const root = "/maintenance-suggestions";
const views = [
  ["active", "待查看"],
  ["history", "观察记录"],
  ["ignored", "已忽略"],
] as const;

function route(view: MaintenanceView, id?: string, before?: string | null) {
  const query = new URLSearchParams();
  if (view !== "active") query.set("view", view);
  if (id) query.set("id", id);
  if (before) query.set("before", before);
  return query.size ? `${root}?${query}` : root;
}

const date = new Intl.DateTimeFormat("zh-CN", {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});
function Time({ value }: { value: number }) {
  const when = new Date(value * 1000);
  return <time dateTime={when.toISOString()}>{date.format(when)}</time>;
}
const statusLabels = {
  active: "异常仍在观察中",
  not_observed: "暂未观察到异常",
  recovered: "已证实恢复",
};
function Status({ item }: { item: Suggestion }) {
  return (
    <span className="maint-status" data-state={item.status}>
      {statusLabels[item.status]}
    </span>
  );
}
function source(item: Suggestion) {
  return item.kind === "feishu_scan_stalled" ? "飞书扫描" : "Task 执行";
}
function message(error: unknown) {
  return error instanceof Error ? error.message : "暂时无法处理维护建议。";
}

type ShareIntent = {
  suggestion: Suggestion;
} & (
  | { target: "main" }
  | { target: "new_discussion" }
  | { target: "discussion"; chat: string; title: string }
);
type ShareState =
  | { status: "sending" | "uncertain"; intent: ShareIntent }
  | { status: "saved"; receipt: ShareReceipt }
  | { status: "rejected"; reason: string };

function shareDestination(intent: ShareIntent) {
  switch (intent.target) {
    case "main":
      return "转给原主 Chat";
    case "new_discussion":
      return "新建讨论并带入";
    case "discussion":
      return `带入讨论「${intent.title}」`;
  }
}

export function MaintenanceSuggestions({
  search,
  onReachable,
}: {
  search: string;
  onReachable: (reachable: boolean) => void;
}) {
  const query = new URLSearchParams(search);
  const view =
    views.find(([key]) => key === query.get("view"))?.[0] ?? "active";
  const before = query.get("before");
  const selected = query.get("id");
  const listKey = `${view}:${before ?? ""}`;
  const [loaded, setLoaded] = useState<{
    key: string;
    page: MaintenancePage;
  } | null>(null);
  const [detail, setDetail] = useState<MaintenanceDetail | null>(null);
  const [listError, setListError] = useState("");
  const [detailError, setDetailError] = useState<{
    id: string;
    text: string;
  }>();
  const [reload, setReload] = useState(0);
  // Category, pagination and detail navigation must not discard a submitted operation.
  const [sharing, setSharing] = useState<ShareState>();
  const pending =
    sharing?.status === "sending" || sharing?.status === "uncertain"
      ? sharing
      : undefined;
  const page = loaded?.key === listKey ? loaded.page : null;
  const target = selected ?? page?.suggestions[0]?.id;
  const shown = detail?.suggestion.id === target ? detail : null;
  const rescan = () => setReload((value) => value + 1);

  async function share(intent: ShareIntent) {
    if (pending && (pending.status === "sending" || pending.intent !== intent))
      return;
    setSharing({ status: "sending", intent });
    try {
      const receipt =
        intent.target === "main"
          ? await handoffMaintenance(intent.suggestion)
          : await discussMaintenance(
              intent.suggestion,
              intent.target === "discussion" ? intent.chat : undefined,
            );
      setSharing({ status: "saved", receipt });
      if (receipt.chat_id !== "main") notifyDiscussionChanged(receipt.chat_id);
    } catch (error) {
      setSharing(
        error instanceof MaintenanceRejection
          ? { status: "rejected", reason: error.message }
          : { status: "uncertain", intent },
      );
    } finally {
      rescan();
    }
  }

  useEffect(() => {
    const controller = new AbortController();
    let pending = false;
    async function load() {
      if (pending) return;
      pending = true;
      try {
        const page = await listMaintenance(view, before, controller.signal);
        if (!controller.signal.aborted) {
          setLoaded({ key: listKey, page });
          setListError("");
          onReachable(true);
        }
      } catch (error) {
        if (!controller.signal.aborted) setListError(message(error));
      } finally {
        pending = false;
      }
    }
    void load();
    const interval = window.setInterval(() => void load(), 15_000);
    return () => {
      controller.abort();
      window.clearInterval(interval);
    };
  }, [view, before, listKey, reload, onReachable]);

  // Observe the selected record even when it moves to another tab.
  useEffect(() => {
    if (!target) return;
    const id = target;
    const controller = new AbortController();
    let pending = false;
    async function load() {
      if (pending) return;
      pending = true;
      try {
        const detail = await readMaintenance(id, controller.signal);
        if (!controller.signal.aborted) {
          setDetail(detail);
          setDetailError(undefined);
        }
      } catch (error) {
        if (!controller.signal.aborted)
          setDetailError({ id, text: message(error) });
      } finally {
        pending = false;
      }
    }
    void load();
    const interval = window.setInterval(() => void load(), 15_000);
    return () => {
      controller.abort();
      window.clearInterval(interval);
    };
  }, [target, reload]);

  return (
    <main className="resource-page maint-page" data-deep={!!selected}>
      <div className="maint-shell">
        <div className="maint-overview">
          <div>
            <h2>系统观测</h2>
            <p>从已确认的运行事实发现异常。每条建议都由你决定如何处理。</p>
          </div>
          <div className="maint-coverage">
            <span>Task 停止证据</span>
            <span>
              {page?.feishu_configured === false
                ? "飞书扫描未配置"
                : "飞书扫描检查点"}
            </span>
          </div>
        </div>
        {pending && (
          <section className="maint-reconcile" aria-label="分享查证">
            {pending.status === "sending" ? (
              <p role="status">正在提交原说明…</p>
            ) : (
              <p role="alert">
                没有收到分享回执，说明可能已保存。查证前不能分享另一份说明。
              </p>
            )}
            <p>
              <a href={route(view, pending.intent.suggestion.id, before)}>
                {pending.intent.suggestion.fault.title}
              </a>
              {` · 证据版本 ${pending.intent.suggestion.revision} · ${shareDestination(pending.intent)}`}
            </p>
            <button
              className="maint-primary"
              type="button"
              disabled={pending.status === "sending"}
              onClick={() => void share(pending.intent)}
            >
              重试原提交
            </button>
          </section>
        )}
        {sharing?.status === "rejected" && (
          <p role="alert" className="maint-reconcile">
            原提交未保存。{sharing.reason}
          </p>
        )}
        {sharing?.status === "saved" && (
          <p role="status" className="maint-reconcile maint-result">
            已保存说明。
            <a href={sharing.receipt.href}>
              打开{sharing.receipt.chat_id === "main" ? "原主 Chat" : "讨论"} ↗
            </a>
          </p>
        )}
        <div className="maint-grid">
          <section className="maint-list" aria-label="建议列表">
            <nav className="maint-tabs" aria-label="建议分类">
              {views.map(([key, label]) => (
                <a
                  key={key}
                  href={route(key)}
                  aria-current={view === key ? "page" : undefined}
                >
                  {label}
                </a>
              ))}
            </nav>
            {listError && <p role="alert">{listError}</p>}
            {!page && !listError && <p role="status">正在读取维护建议…</p>}
            {page && !page.suggestions.length && (
              <p className="maint-empty">
                {view === "active"
                  ? "没有待查看的维护建议。"
                  : view === "history"
                    ? "还没有异常消失或恢复记录。"
                    : "没有已忽略的建议。"}
              </p>
            )}
            <div className="maint-items">
              {page?.suggestions.map((item) => (
                <a
                  href={route(view, item.id, before)}
                  className="maint-item"
                  key={item.id}
                  aria-current={target === item.id ? "true" : undefined}
                >
                  <span className="maint-item-source">{source(item)}</span>
                  <strong>{item.fault.title}</strong>
                  <span className="maint-item-summary">
                    {item.fault.impact}
                  </span>
                  <Status item={item} />
                </a>
              ))}
            </div>
            <div className="maint-pages">
              {before && <a href={route(view)}>返回首页</a>}
              {page?.next_before && (
                <a href={route(view, undefined, page.next_before)}>下一页</a>
              )}
            </div>
          </section>
          <section className="maint-inspect" aria-label="建议详情">
            {selected && (
              <a className="maint-back" href={route(view, undefined, before)}>
                ← 返回建议列表
              </a>
            )}
            {detailError?.id === target && detailError && (
              <p role="alert">{detailError.text}</p>
            )}
            {shown ? (
              <SuggestionDetail
                key={shown.suggestion.id}
                detail={shown}
                refresh={rescan}
                sharePending={!!pending}
                submitShare={(intent) => void share(intent)}
                pinForShare={() => {
                  if (!selected) replacePageQuery({ id: shown.suggestion.id });
                  if (!pending) setSharing(undefined);
                }}
              />
            ) : (
              target &&
              detailError?.id !== target && <p role="status">正在读取证据…</p>
            )}
          </section>
        </div>
      </div>
    </main>
  );
}

function SuggestionDetail({
  detail,
  pinForShare,
  refresh,
  sharePending,
  submitShare,
}: {
  detail: MaintenanceDetail;
  pinForShare: () => void;
  refresh: () => void;
  sharePending: boolean;
  submitShare: (intent: ShareIntent) => void;
}) {
  const item = detail.suggestion;
  const [form, setForm] = useState<"discussion" | "handoff">();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [discussions, setDiscussions] = useState<Discussion[]>();
  const [discussionsError, setDiscussionsError] = useState("");
  const [chat, setChat] = useState("");
  // The selected, previewed version stays explicit even if polling sees an update.
  const [preview, setPreview] = useState(detail);
  const changed = preview.suggestion.revision !== item.revision;

  useEffect(() => {
    if (form !== "discussion") return;
    const controller = new AbortController();
    void listDiscussions(false, controller.signal)
      .then((page) => {
        if (!controller.signal.aborted) setDiscussions(page.discussions);
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted) setDiscussionsError(message(error));
      });
    return () => controller.abort();
  }, [form]);

  function open(target: typeof form) {
    pinForShare();
    setPreview(detail);
    setForm(target);
    setError("");
    setDiscussionsError("");
    setChat("");
  }

  async function ignore() {
    if (busy || sharePending) return;
    setBusy(true);
    setError("");
    try {
      await ignoreMaintenance(item);
      setForm(undefined);
    } catch (error) {
      setError(message(error));
    } finally {
      setBusy(false);
      refresh();
    }
  }

  function confirmShare() {
    if (busy || sharePending || changed || !form) return;
    const suggestion = preview.suggestion;
    const destination: ShareIntent =
      form === "handoff"
        ? { target: "main", suggestion }
        : chat
          ? {
              target: "discussion",
              suggestion,
              chat,
              title:
                discussions?.find((item) => item.chat_id === chat)?.title ??
                chat,
            }
          : { target: "new_discussion", suggestion };
    setForm(undefined);
    setError("");
    submitShare(destination);
  }

  return (
    <article className="maint-card" aria-labelledby="maint-title">
      <div className="maint-eyebrow">
        <Status item={item} />
        <span>{source(item)}</span>
        {item.ignored && <span>已忽略此版本</span>}
      </div>
      <h2 id="maint-title">{item.fault.title}</h2>
      <section className="maint-impact" aria-label="影响">
        <span>影响</span>
        <p>{item.fault.impact}</p>
      </section>
      <dl className="maint-times">
        <div>
          <dt>首次观察</dt>
          <dd>
            <Time value={item.first_seen} />
          </dd>
        </div>
        <div>
          <dt>最近异常观察</dt>
          <dd>
            <Time value={item.last_seen} />
          </dd>
        </div>
        <div>
          <dt>证据更新 · 版本 {item.revision}</dt>
          <dd>
            <Time value={item.changed_at} />
          </dd>
        </div>
      </dl>
      <section className="maint-evidence" aria-labelledby="maint-evidence">
        <h3 id="maint-evidence">来源与证据</h3>
        <p>{item.fault.known}</p>
        <div className="maint-links">
          {item.fault.links.map((link) => (
            <a key={link.href} href={link.href}>
              {link.label} ↗
            </a>
          ))}
        </div>
        {item.followup && (
          <div className="maint-followup">
            <h4>后续观察</h4>
            <p>{item.followup.known}</p>
            {item.followup.link && (
              <a href={item.followup.link.href}>{item.followup.link.label} ↗</a>
            )}
          </div>
        )}
        <p className="maint-unknown">
          <strong>尚未确定</strong> {item.fault.unknown}
        </p>
      </section>
      <section className="maint-actions" aria-labelledby="maint-actions">
        <h3 id="maint-actions">人工处置</h3>
        <p>只转入证据说明，不会自动启动调查或改变原 Issue。</p>
        <div className="maint-buttons">
          <button
            type="button"
            onClick={() => open("discussion")}
            disabled={busy || sharePending}
          >
            带入讨论
          </button>
          <button
            type="button"
            onClick={() => open("handoff")}
            disabled={busy || sharePending}
          >
            转给原主 Chat
          </button>
          <button
            type="button"
            onClick={() => void ignore()}
            disabled={busy || sharePending}
          >
            {item.ignored ? "撤销忽略" : "忽略此版本"}
          </button>
        </div>
        {error && <p role="alert">{error}</p>}
        {form && !sharePending && (
          <div className="maint-share">
            {changed && (
              <p role="status">
                已出现新证据。
                <button type="button" onClick={() => setPreview(detail)}>
                  查看最新版本
                </button>
              </p>
            )}
            {form === "discussion" ? (
              <>
                <label htmlFor="maint-discussion">选择讨论</label>
                <select
                  id="maint-discussion"
                  value={chat}
                  disabled={busy}
                  onChange={(event) => setChat(event.target.value)}
                >
                  <option value="">新建讨论</option>
                  {discussions?.map((discussion) => (
                    <option key={discussion.chat_id} value={discussion.chat_id}>
                      {discussion.title}
                    </option>
                  ))}
                </select>
                {discussionsError && <p role="alert">{discussionsError}</p>}
              </>
            ) : (
              <div>
                <h4>发送前查看说明</h4>
                <Markdown text={preview.note} variant="chat" maintenanceLinks />
              </div>
            )}
            <div className="maint-buttons">
              <button
                className="maint-primary"
                type="button"
                disabled={busy || changed}
                onClick={confirmShare}
              >
                {form === "handoff"
                  ? "确认转给原主 Chat"
                  : chat
                    ? "带入所选讨论"
                    : "新建讨论并带入"}
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() => setForm(undefined)}
              >
                取消
              </button>
            </div>
          </div>
        )}
      </section>
    </article>
  );
}
