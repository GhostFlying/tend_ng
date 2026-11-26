import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { issueLink, readAgents, readExecutionHosts } from "./api";
import type {
  AgentDirectory,
  AgentProfile,
  ExecutionHost,
  ExecutionRunner,
  RunnerInstance,
  RunningTask,
} from "./api";
import { replacePageQuery, usePageURL } from "./navigation";
import "./resources.css";
import "./execution-hosts.css";

type Props = { onReachable: (online: boolean) => void };

function useResource<T>(
  props: Props,
  read: (signal: AbortSignal) => Promise<T>,
) {
  const { onReachable } = props;
  const [data, setData] = useState<T>();
  const [failed, setFailed] = useState(false);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    let timer: number;
    async function update() {
      try {
        const value = await read(controller.signal);
        if (controller.signal.aborted) return;
        setData(value);
        setFailed(false);
        onReachable(true);
      } catch {
        if (controller.signal.aborted) return;
        setFailed(true);
        onReachable(false);
      }
      timer = window.setTimeout(() => void update(), 5000);
    }
    void update();
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [onReachable, read, revision]);

  return { data, failed, refresh: () => setRevision((value) => value + 1) };
}

const categories = [
  {
    id: "issue-router",
    description: "拆分、委派并汇总顶层 Issue。",
  },
  {
    id: "implementation",
    description: "实现并验证工作子 Issue。",
  },
  {
    id: "implementation-economy",
    description: "实施范围和验收已明确的任务。",
  },
  {
    id: "investigation",
    description: "查证问题、影响范围和运行事实。",
  },
  { id: "review", description: "对指定交付进行独立审阅。" },
] as const;

type Entry = {
  key: string;
  agent: AgentDirectory["router"] & Partial<AgentProfile>;
  runner?: RunnerInstance;
  running?: number;
};

function currentRunners(runners: RunnerInstance[]) {
  return runners.filter((runner) => runner.retired_at == null);
}

function entries(data: AgentDirectory): Entry[] {
  const work = currentRunners(data.runners).flatMap((runner) =>
    runner.profile.agents.map((agent) => ({
      key: `${runner.profile.runner_id}:${agent.id}`,
      agent,
      runner,
      running:
        data.activity.find(
          (row) =>
            row.runner_id === runner.profile.runner_id &&
            row.agent_id === agent.id &&
            row.revision === agent.revision,
        )?.running ?? 0,
    })),
  );
  const all: Entry[] = [{ key: "router", agent: data.router }, ...work];
  const busy = (entry: Entry) =>
    Number(!!entry.runner?.online && (entry.running ?? 0) > 0);
  return all.sort((a, b) => busy(b) - busy(a));
}

function runtimeName(entry: Entry) {
  return entry.agent.app_server
    ? `Codex · ${entry.agent.app_server}`
    : "Codex · app-server";
}

function activity(entry: Entry, failed: boolean) {
  if (entry.running === undefined) return "任务数未提供";
  if (!entry.runner?.online || failed) return `记录运行任务 ${entry.running}`;
  return entry.running ? `${entry.running} 个任务运行中` : "0 个任务 · 空闲";
}

function agentObservation(entry: Entry, failed: boolean) {
  if (!entry.runner) return "未接入运行观测";
  if (failed) return "上次观察";
  return entry.runner.online ? "在线" : "离线";
}

function Refresh({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      className="resource-icon"
      aria-label="重新读取"
      onClick={onClick}
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.7"
        aria-hidden="true"
      >
        <path d="M20 5v6h-6M4 19v-6h6M19.3 10a7.5 7.5 0 0 0-12.7-4L4 9m16 6-2.6 3A7.5 7.5 0 0 1 4.7 14" />
      </svg>
    </button>
  );
}

function AgentRow({
  entry,
  selected,
  failed,
  onClick,
}: {
  entry: Entry;
  selected: boolean;
  failed: boolean;
  onClick: (button: HTMLButtonElement) => void;
}) {
  const { agent, runner } = entry;
  const location = runner?.profile.host_id ?? "服务端";
  const model = `${agent.model ?? "默认模型"} · ${agent.reasoning_effort ?? "默认"}`;
  const state = agentObservation(entry, failed);
  return (
    <button
      type="button"
      className="agent-row"
      aria-pressed={selected}
      onClick={(event) => onClick(event.currentTarget)}
    >
      <span className="agent-identity">
        <strong>{runner ? agent.id : "Router"}</strong>
        <span className="agent-activity">{activity(entry, failed)}</span>
        <small>
          {runner ? runner.profile.runner_id : "主对话、路由 · router"}
        </small>
      </span>
      <span className="agent-desktop-fact">{agent.category ?? "—"}</span>
      <span className="agent-desktop-fact">{location}</span>
      <span className="agent-desktop-fact">{runtimeName(entry)}</span>
      <span className="agent-desktop-fact">{model}</span>
      <span className="agent-state">
        {state === "在线" && <i aria-hidden="true" />}
        {state}
      </span>
      <span className="agent-mobile-facts">
        {runner
          ? `${agent.category ?? "—"} · ${location} · ${runner.profile.runner_id}`
          : "主对话、路由 · 服务端"}
        <small>
          {runtimeName(entry)} · {model}
        </small>
      </span>
      <span className="agent-chevron" aria-hidden="true">
        ›
      </span>
    </button>
  );
}

function Facts({ values }: { values: [string, ReactNode][] }) {
  return (
    <dl className="resource-facts">
      {values.map(([label, value]) => (
        <div key={label}>
          <dt>{label}</dt>
          <dd>{value}</dd>
        </div>
      ))}
    </dl>
  );
}

function AgentDetails({ entry, failed }: { entry: Entry; failed: boolean }) {
  const { agent, runner } = entry;
  return (
    <>
      <h2 tabIndex={-1}>{runner ? agent.id : "Router"}</h2>
      {agent.description && <p>{agent.description}</p>}
      <h3>定义</h3>
      <Facts
        values={[
          ["Agent ID", agent.id],
          [
            runner ? "类别" : "用途",
            runner ? (agent.category ?? "—") : "主对话、任务路由",
          ],
          ["运行位置", runner?.profile.host_id ?? "服务端"],
          ...(runner
            ? [["Runner", runner.profile.runner_id] as [string, string]]
            : []),
          ["运行方式", runtimeName(entry)],
          ["模型", agent.model ?? "默认模型"],
          ["推理", agent.reasoning_effort ?? "默认"],
          ...(agent.revision
            ? [["配置版本", agent.revision] as [string, string]]
            : []),
          ...(agent.capabilities?.length
            ? [["能力", agent.capabilities.join("、")] as [string, string]]
            : []),
        ]}
      />
      <h3>运行观测</h3>
      <Facts
        values={[
          ["观测", agentObservation(entry, failed)],
          ["任务", activity(entry, failed)],
          ...(runner
            ? [["最后心跳", timestamp(runner.last_seen)] as [string, ReactNode]]
            : []),
        ]}
      />
    </>
  );
}

export function Agents(props: Props) {
  const { data, failed, refresh } = useResource(props, readAgents);
  const { searchParams } = usePageURL();
  const categoryTab = searchParams.get("tab") === "categories";
  const search = searchParams.get("q") ?? "";
  const category = searchParams.get("category") ?? "";
  const location = searchParams.get("location") ?? "";
  const selectedKey = searchParams.get("selected") ?? "";
  const scroller = useRef<HTMLElement>(null);
  const detail = useRef<HTMLElement>(null);
  const restore = useRef<{ row: HTMLButtonElement; top: number } | null>(null);
  const all = data ? entries(data) : [];
  const selected = !categoryTab
    ? all.find((entry) => entry.key === selectedKey)
    : undefined;
  const selectedId = selected?.key;
  const hosts = [
    ...new Set(
      all.flatMap((entry) =>
        entry.runner ? [entry.runner.profile.host_id] : [],
      ),
    ),
  ];
  const methods = [...new Set(all.map(runtimeName))];
  const query = search.trim().toLowerCase();
  const visible = all.filter((entry) => {
    const host = entry.runner?.profile.host_id ?? "服务端";
    return (
      (!category || entry.agent.category === category) &&
      (!location ||
        location === `host:${host}` ||
        location === `method:${runtimeName(entry)}`) &&
      `${entry.agent.id} ${host} ${entry.runner?.profile.runner_id ?? ""}`
        .toLowerCase()
        .includes(query)
    );
  });

  useEffect(() => {
    if (!window.matchMedia("(max-width: 1399px)").matches) return;
    if (selectedId) {
      if (scroller.current) scroller.current.scrollTop = 0;
      detail.current?.querySelector("h2")?.focus({ preventScroll: true });
    } else if (restore.current) {
      if (scroller.current) scroller.current.scrollTop = restore.current.top;
      restore.current.row.focus({ preventScroll: true });
      restore.current = null;
    }
  }, [selectedId]);

  function switchTab(tab: boolean) {
    replacePageQuery({ tab: tab ? "categories" : "", q: "", selected: "" });
  }

  function tabKeys(event: KeyboardEvent<HTMLDivElement>) {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const next =
      event.key === "Home" ? false : event.key === "End" ? true : !categoryTab;
    switchTab(next);
    event.currentTarget.querySelectorAll("button")[Number(next)]?.focus();
  }

  return (
    <main
      className="resource-page agent-directory"
      aria-label="Agents"
      ref={scroller}
    >
      {failed && (
        <p className="resource-warning" role="status">
          {data ? "暂时无法读取，显示上次结果。" : "暂时无法读取 Agent。"}
        </p>
      )}
      <div className="agent-catalog" data-detail={!!selected}>
        <div
          className="agent-tabs"
          role="tablist"
          aria-label="目录内容"
          onKeyDown={tabKeys}
        >
          <button
            type="button"
            role="tab"
            aria-selected={!categoryTab}
            tabIndex={categoryTab ? -1 : 0}
            onClick={() => switchTab(false)}
          >
            Agents
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={categoryTab}
            tabIndex={categoryTab ? 0 : -1}
            onClick={() => switchTab(true)}
          >
            Categories
          </button>
        </div>
        <div className="agent-grid" data-selected={!!selected}>
          <div>
            <div className="agent-toolbar">
              <label className="agent-search">
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.7"
                  aria-hidden="true"
                >
                  <circle cx="10.5" cy="10.5" r="6.5" />
                  <path d="m16 16 4 4" />
                </svg>
                <input
                  type="search"
                  placeholder={categoryTab ? "搜索类别" : "搜索名称或 ID"}
                  aria-label={categoryTab ? "搜索类别" : "搜索名称或 ID"}
                  value={search}
                  onChange={(event) =>
                    replacePageQuery({ q: event.target.value, selected: "" })
                  }
                />
              </label>
              {!categoryTab && (
                <>
                  <select
                    aria-label="类别筛选"
                    value={category}
                    onChange={(event) =>
                      replacePageQuery({
                        category: event.target.value,
                        selected: "",
                      })
                    }
                  >
                    <option value="">全部类别</option>
                    {categories.map(({ id }) => (
                      <option key={id} value={id}>
                        {id}
                      </option>
                    ))}
                  </select>
                  <select
                    aria-label="运行位置或方式筛选"
                    value={location}
                    onChange={(event) =>
                      replacePageQuery({
                        location: event.target.value,
                        selected: "",
                      })
                    }
                  >
                    <option value="">全部位置</option>
                    <optgroup label="运行位置">
                      {["服务端", ...hosts].map((host) => (
                        <option key={host} value={`host:${host}`}>
                          {host}
                        </option>
                      ))}
                    </optgroup>
                    <optgroup label="运行方式">
                      {methods.map((method) => (
                        <option key={method} value={`method:${method}`}>
                          {method}
                        </option>
                      ))}
                    </optgroup>
                  </select>
                </>
              )}
              <Refresh onClick={refresh} />
            </div>
            {categoryTab ? (
              <div role="tabpanel" aria-label="Categories">
                <div className="agent-category-row agent-table-head">
                  <span>类别</span>
                  <span>说明</span>
                  <span>状态</span>
                  <span>已登记 Agent</span>
                </div>
                {categories
                  .filter((item) =>
                    `${item.id} ${item.description}`
                      .toLowerCase()
                      .includes(query),
                  )
                  .map((item) => (
                    <div className="agent-category-row" key={item.id}>
                      <strong>{item.id}</strong>
                      <span>{item.description}</span>
                      <span>启用</span>
                      <div className="agent-category-names">
                        {all
                          .filter((entry) => entry.agent.category === item.id)
                          .map((entry) => (
                            <button
                              className="text-button"
                              key={entry.key}
                              type="button"
                              onClick={() =>
                                replacePageQuery({
                                  tab: "",
                                  q: "",
                                  category: item.id,
                                  location: "",
                                  selected: entry.key,
                                })
                              }
                            >
                              {entry.agent.id} ·{" "}
                              {entry.runner?.profile.runner_id}
                            </button>
                          ))}
                      </div>
                    </div>
                  ))}
              </div>
            ) : (
              <div role="tabpanel" aria-label="Agents">
                <p className="agent-activity-note">
                  运行任务优先，组内保留目录顺序。任务数按 Runner
                  上报的实际运行记录计数。
                </p>
                <div className="agent-row agent-table-head">
                  <span>Agent</span>
                  <span>类别</span>
                  <span>运行位置</span>
                  <span>运行方式</span>
                  <span>模型 / 推理</span>
                  <span>观测</span>
                </div>
                {!data && !failed && (
                  <p className="resource-empty" role="status">
                    正在读取 Agent…
                  </p>
                )}
                {data && !visible.length && (
                  <p className="resource-empty">没有符合筛选条件的 Agent。</p>
                )}
                {visible.map((entry) => (
                  <AgentRow
                    key={entry.key}
                    entry={entry}
                    selected={selectedId === entry.key}
                    failed={failed}
                    onClick={(row) => {
                      restore.current = {
                        row,
                        top: scroller.current?.scrollTop ?? 0,
                      };
                      replacePageQuery({
                        selected: selectedId === entry.key ? "" : entry.key,
                      });
                    }}
                  />
                ))}
              </div>
            )}
          </div>
          {selected && (
            <aside
              className="agent-inspector"
              aria-label={`${selected.agent.id} 详情`}
            >
              <AgentDetails entry={selected} failed={failed} />
            </aside>
          )}
        </div>
        <p className="agent-footer">定义由对应环境维护，此页只读。</p>
      </div>
      {selected && (
        <section
          className="agent-standalone"
          ref={detail}
          aria-label="Agent 详情"
        >
          <button
            type="button"
            className="text-button agent-back"
            onClick={() => replacePageQuery({ selected: "" })}
          >
            ‹ 返回列表
          </button>
          <AgentDetails entry={selected} failed={failed} />
        </section>
      )}
    </main>
  );
}

function timestamp(seconds: number) {
  const value = new Date(seconds * 1000);
  return (
    <time dateTime={value.toISOString()}>
      {value.toLocaleString("zh-CN", { hour12: false })}
    </time>
  );
}

const runnerModes: Record<ExecutionRunner["mode"], string> = {
  serving: "接单中",
  draining: "收尾中",
  standby: "待接替",
  drained: "已排空",
};

const taskNames: Record<string, string> = {
  "issue-router": "协调 Issue",
  implementation: "实施",
  "implementation-economy": "实施",
  investigation: "调查",
  review: "审阅",
};

function runnerName(id: string) {
  // Generated registry IDs need a short visual handle, not a long UUID row.
  const generated = /^runner-([\da-f]{8})-(?:[\da-f]{4}-){3}[\da-f]{12}$/i.exec(
    id,
  );
  return generated ? `Runner · ${generated[1].slice(0, 6)}` : id;
}

function elapsed(started: number | null, observed: number) {
  if (started === null) return null;
  const seconds = Math.max(0, observed - started);
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分${seconds % 60} 秒`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时 ${minutes % 60} 分`;
  return `${Math.floor(hours / 24)} 天 ${hours % 24} 小时`;
}

function RunningTasks({
  runs,
  observedAt,
}: {
  runs: RunningTask[];
  observedAt: number;
}) {
  const groups = new Map<string, RunningTask[]>();
  for (const run of runs) {
    const group = groups.get(run.root_issue.id);
    if (group) group.push(run);
    else groups.set(run.root_issue.id, [run]);
  }
  return (
    <div className="host-run-table">
      <div className="host-run-guide" aria-hidden="true">
        <span>Issue / Task</span>
        <span>执行者</span>
        <span>已运行</span>
      </div>
      {[...groups].map(([id, group]) => {
        const root = group[0].root_issue;
        return (
          <section className="host-run-group" key={id}>
            <h4 className="host-issue">
              <a href={issueLink(id, root.short_number ?? undefined)}>
                {root.short_number !== null && (
                  <span>#{root.short_number} </span>
                )}
                {root.title}
              </a>
            </h4>
            <ul className="host-task-list">
              {group.map((run) => {
                const direct = run.issue.id === root.id;
                const duration = elapsed(run.started_at, observedAt);
                return (
                  <li key={run.run_id} className="host-task">
                    <span className="host-task-name">
                      <small>Task</small>
                      {direct ? (
                        (taskNames[run.category] ?? run.category)
                      ) : (
                        <a
                          href={issueLink(
                            run.issue.id,
                            run.issue.short_number ?? undefined,
                          )}
                        >
                          {run.issue.title}
                        </a>
                      )}
                    </span>
                    <span
                      className="host-task-agent"
                      aria-label={`执行者 ${run.agent_id}`}
                    >
                      {run.agent_id}
                    </span>
                    <span
                      className="host-run-time"
                      aria-label={
                        duration ? `已运行 ${duration}` : "启动时间未知"
                      }
                    >
                      {duration ?? "—"}
                    </span>
                  </li>
                );
              })}
            </ul>
          </section>
        );
      })}
    </div>
  );
}

function RunnerActivity({
  runner,
  observedAt,
}: {
  runner: ExecutionRunner;
  observedAt: number;
}) {
  const [chosen, setChosen] = useState<boolean | null>(null);
  const open = chosen ?? runner.runs.length > 0;
  return (
    <details className="host-runner-block" open={open}>
      <summary
        className="host-runner-row"
        onClick={(event) => {
          event.preventDefault();
          setChosen(!open);
        }}
      >
        <span className="host-runner-id">
          <strong>{runnerName(runner.runner_id)}</strong>
          <span
            className="host-runner-mode"
            data-mode={runner.online ? runner.mode : "offline"}
          >
            {runner.online ? runnerModes[runner.mode] : "离线"}
          </span>
        </span>
        <span className="host-runner-use">
          <b>{runner.running_slots ?? "—"}</b> / {runner.slot_limit} 槽位
        </span>
        <span className="host-runner-chevron" aria-hidden="true">
          ›
        </span>
      </summary>
      {open && (
        <div className="host-run-content">
          {runner.runs.length > 0 && (
            <RunningTasks runs={runner.runs} observedAt={observedAt} />
          )}
          {runner.running_slots === null ? (
            <p className="host-run-empty">离线，无法确认正在运行的 Task。</p>
          ) : runner.running_slots === 0 ? (
            <p className="host-run-empty">当前没有运行中的 Task。</p>
          ) : runner.running_slots > runner.runs.length ? (
            <p className="host-run-empty">有运行记录尚未同步。</p>
          ) : null}
        </div>
      )}
    </details>
  );
}

function HostCard({
  host,
  observedAt,
}: {
  host: ExecutionHost;
  observedAt: number;
}) {
  const connected = host.runners.some((runner) => runner.online);
  const draining = host.runners.reduce(
    (sum, runner) =>
      sum + (runner.mode === "draining" ? (runner.running_slots ?? 0) : 0),
    0,
  );
  const capacity = host.slot_limit;
  const slots = Math.min(capacity, 24);
  const fill = capacity
    ? Math.min(slots, Math.ceil((slots * host.running_slots) / capacity))
    : 0;
  const serving = capacity
    ? Math.ceil((slots * (host.running_slots - draining)) / capacity)
    : 0;
  return (
    <section
      className="execution-host-card"
      aria-label={`主机 ${host.host_id}`}
    >
      <header className="execution-host-head">
        <div className="execution-host-name">
          <span className="execution-host-icon" aria-hidden="true">
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.6"
              strokeLinecap="round"
            >
              <rect x="3" y="5" width="18" height="6" rx="2" />
              <rect x="3" y="13" width="18" height="6" rx="2" />
              <path d="M7 8h.1M7 16h.1" />
            </svg>
          </span>
          <div>
            <h2>{host.host_id}</h2>
            <p className="execution-host-meta">
              <span data-online={connected}>{connected ? "在线" : "离线"}</span>
              <span aria-hidden="true">·</span>
              <span>{host.runners.length} 个 Runner</span>
            </p>
          </div>
        </div>
        <div className="execution-host-count">
          <span>
            <strong>
              {connected ? host.running_slots : "—"}
              <small> / {capacity || "—"}</small>
            </strong>
            <small>槽位</small>
          </span>
          {slots > 0 && (
            <span className="execution-host-meter" aria-hidden="true">
              {Array.from({ length: slots }, (_, index) => (
                <i
                  key={index}
                  data-used={index < fill}
                  data-draining={index >= serving && index < fill}
                />
              ))}
            </span>
          )}
        </div>
      </header>
      {host.runners.map((runner) => (
        <RunnerActivity
          key={runner.runner_id}
          runner={runner}
          observedAt={observedAt}
        />
      ))}
    </section>
  );
}

export function Hosts(props: Props) {
  const { data, failed } = useResource(props, readExecutionHosts);
  const connected = data?.hosts.filter((host) =>
    host.runners.some((runner) => runner.online),
  ).length;
  return (
    <main className="resource-page hosts-page" aria-label="执行主机占用">
      <div className="execution-host-lane">
        {data && (
          <p className="host-list-title">
            执行主机 · <strong>{data.hosts.length}</strong> 台
            <span aria-hidden="true"> · </span>
            {connected} 台在线
          </p>
        )}
        {failed && (
          <p className="resource-warning" role="status">
            {data
              ? "等待连接恢复，显示最后观察状态。"
              : "暂时无法读取执行主机。"}
          </p>
        )}
        {!data && !failed && (
          <p role="status" className="resource-empty">
            正在读取…
          </p>
        )}
        {data && !data.hosts.length && (
          <p className="resource-empty">暂无已登记的执行主机。</p>
        )}
        <div className="execution-host-list">
          {data?.hosts.map((host) => (
            <HostCard
              key={host.host_id}
              host={host}
              observedAt={data.observed_at}
            />
          ))}
        </div>
      </div>
    </main>
  );
}
