import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { readAgents, readRunners } from "./api";
import type { AgentDirectory, AgentProfile, RunnerInstance } from "./api";
import "./resources.css";

type Props = { token: string; onReachable: (online: boolean) => void };

function useResource<T>(
  props: Props,
  read: (token: string, signal: AbortSignal) => Promise<T>,
) {
  const { token, onReachable } = props;
  const [data, setData] = useState<T>();
  const [failed, setFailed] = useState(false);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    let timer: number;
    async function update() {
      try {
        const value = await read(token, controller.signal);
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
  }, [token, onReachable, read, revision]);

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

function entries(data: AgentDirectory): Entry[] {
  const work = data.runners.flatMap((runner) =>
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

function runnerMode(runner: RunnerInstance) {
  if (runner.retired_at) return "已退役";
  if (runner.drained_at) return "已排空";
  if (runner.successor) return "排空中";
  if (runner.profile.standby) return "待接替";
  return "正常";
}

function activity(entry: Entry, failed: boolean) {
  if (entry.running === undefined) return "任务数未提供";
  if (!entry.runner?.online || failed) return `记录运行任务 ${entry.running}`;
  return entry.running ? `${entry.running} 个任务运行中` : "0 个任务 · 空闲";
}

function agentObservation(entry: Entry, failed: boolean) {
  if (!entry.runner) return "未接入运行观测";
  if (entry.runner.retired_at) return "已退役";
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
  const initial = new URLSearchParams(useRef(window.location.search).current);
  const [categoryTab, setCategoryTab] = useState(
    initial.get("tab") === "categories",
  );
  const [search, setSearch] = useState(initial.get("q") ?? "");
  const [category, setCategory] = useState(initial.get("category") ?? "");
  const [location, setLocation] = useState(initial.get("location") ?? "");
  const [selectedKey, setSelected] = useState(initial.get("selected") ?? "");
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
    const url = new URL(window.location.href);
    for (const [key, value] of Object.entries({
      tab: categoryTab ? "categories" : "",
      q: search,
      category,
      location,
      selected: selectedKey,
    })) {
      if (value) url.searchParams.set(key, value);
      else url.searchParams.delete(key);
    }
    window.history.replaceState(null, "", url);
  }, [categoryTab, search, category, location, selectedKey]);

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
    setCategoryTab(tab);
    setSearch("");
    setSelected("");
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
                  onChange={(event) => {
                    setSearch(event.target.value);
                    setSelected("");
                  }}
                />
              </label>
              {!categoryTab && (
                <>
                  <select
                    aria-label="类别筛选"
                    value={category}
                    onChange={(event) => {
                      setCategory(event.target.value);
                      setSelected("");
                    }}
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
                    onChange={(event) => {
                      setLocation(event.target.value);
                      setSelected("");
                    }}
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
                              onClick={() => {
                                switchTab(false);
                                setCategory(item.id);
                                setLocation("");
                                setSelected(entry.key);
                              }}
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
                      setSelected(selectedId === entry.key ? "" : entry.key);
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
            onClick={() => setSelected("")}
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

function hostMetrics(runners: RunnerInstance[], failed: boolean) {
  const current = runners.filter((runner) => !runner.retired_at);
  const connected = current.filter((runner) => runner.online);
  const active = connected.length > 0 && !failed;
  const observed = active ? connected : current;
  return {
    active,
    state: failed
      ? "上次观察"
      : active
        ? "在线"
        : current.length
          ? "离线"
          : "已退役",
    running: observed.reduce(
      (total, runner) => total + runner.profile.running_slots,
      0,
    ),
    slots: observed.reduce(
      (total, runner) => total + runner.profile.slot_limit,
      0,
    ),
  };
}

function HostDetails({
  id,
  runners,
  failed,
  close,
}: {
  id: string;
  runners: RunnerInstance[];
  failed: boolean;
  close: () => void;
}) {
  const panel = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    panel.current?.showModal();
  }, []);
  const host = hostMetrics(runners, failed);

  return (
    <dialog
      ref={panel}
      className="host-dialog"
      aria-label="主机详情"
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <div className="host-dialog-header">
        <h2>主机详情</h2>
        <button
          className="resource-icon"
          type="button"
          aria-label="关闭主机详情"
          onClick={close}
        >
          ×
        </button>
      </div>
      <h3>{id}</h3>
      <Facts
        values={[
          ["连接", host.state],
          [
            host.active ? "槽位占用" : "记录槽位",
            `${host.running} / ${host.slots}`,
          ],
          ["Runner", runners.length],
        ]}
      />
      {runners.map((runner) => (
        <section className="host-runner" key={runner.profile.runner_id}>
          <h4>{runner.profile.runner_id}</h4>
          <Facts
            values={[
              ["连接", failed ? "上次观察" : runner.online ? "在线" : "离线"],
              ["运行模式", runnerMode(runner)],
              [
                "槽位占用",
                `${runner.profile.running_slots} / ${runner.profile.slot_limit}`,
              ],
              [
                "可派发槽位",
                !failed && runner.online && runnerMode(runner) === "正常"
                  ? runner.profile.available_for_assignment
                  : "—",
              ],
              ["最后心跳", timestamp(runner.last_seen)],
              ["通信协议", `v${runner.protocol}`],
              ["仓库", runner.profile.repos.join("、") || "—"],
              ...(runner.successor
                ? [["接替 Runner", runner.successor] as [string, string]]
                : []),
            ]}
          />
        </section>
      ))}
    </dialog>
  );
}

export function Hosts(props: Props) {
  const { data, failed } = useResource(props, readRunners);
  const [selected, setSelected] = useState("");
  const restore = useRef<HTMLButtonElement>(null);
  const hosts = new Map<string, RunnerInstance[]>();
  for (const runner of data ?? []) {
    const id = runner.profile.host_id;
    const prior = hosts.get(id) ?? [];
    hosts.set(id, [...prior, runner]);
  }
  const selectedRunners = hosts.get(selected);
  return (
    <main className="resource-page hosts-page" aria-label="执行主机占用">
      <p className="host-list-title">执行主机{data && ` · ${hosts.size} 台`}</p>
      {failed && (
        <p className="resource-warning" role="status">
          {data ? "等待连接恢复，显示最后观察状态。" : "暂时无法读取执行主机。"}
        </p>
      )}
      {!data && !failed && (
        <p role="status" className="resource-empty">
          正在读取…
        </p>
      )}
      {data && !hosts.size && (
        <p className="resource-empty">暂无已登记的执行主机。</p>
      )}
      {[...hosts].map(([id, runners]) => {
        const host = hostMetrics(runners, failed);
        const mode = runners.find(
          (runner) => runner.successor && !runner.retired_at,
        );
        return (
          <button
            className="host-row"
            type="button"
            key={id}
            aria-label={`主机详情：${id}`}
            onClick={(event) => {
              restore.current = event.currentTarget;
              setSelected(id);
            }}
          >
            <span className="host-identity">
              <span>{id}</span>
              <small data-online={host.active}>{host.state}</small>
            </span>
            <span className="host-summary">
              {mode && <span className="host-mode">{runnerMode(mode)}</span>}
              {host.active
                ? host.running
                  ? "执行中"
                  : "空闲"
                : "记录占用"} · {host.running} / {host.slots} 槽位
              {runners.length > 1 && ` · ${runners.length} 个 Runner`}
            </span>
          </button>
        );
      })}
      {selectedRunners && (
        <HostDetails
          id={selected}
          runners={selectedRunners}
          failed={failed}
          close={() => {
            setSelected("");
            requestAnimationFrame(() => restore.current?.focus());
          }}
        />
      )}
    </main>
  );
}
