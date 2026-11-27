import { useEffect, useState } from "react";
import {
  discussionLink,
  listDiscussions,
  notifyDiscussionChanged,
} from "./discussionApi";
import type { Discussion } from "./discussionApi";
import {
  feishuCard,
  feishuDiscuss,
  feishuFeed,
  feishuHandoff,
  feishuIgnore,
  feishuSync,
} from "./feishuApi";
import type { FeishuCard, FeishuFeed, FeishuMessage } from "./feishuApi";
import { navigate, trackPageVisit } from "./navigation";
import "./feishu-suggestions.css";

const base = "/feishu-suggestions";
const detailLink = (id: string) => `${base}?id=${encodeURIComponent(id)}`;
const date = new Intl.DateTimeFormat("zh-CN", {
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});
const stamp = (time: number) => date.format(time * 1000);
const errorText = (error: unknown) =>
  error instanceof Error ? error.message : "操作暂时无法完成。";

const tabs = [
  ["open", "待查看"],
  ["resolved", "已解决"],
  ["ignored", "已忽略"],
] as const;
type Tab = (typeof tabs)[number][0];
function tabOf(item: FeishuCard): Tab {
  if (item.ignored) return "ignored";
  return item.evidence.assessment?.disposition === "resolved"
    ? "resolved"
    : "open";
}

function titleOf(item: FeishuCard) {
  return item.evidence.assessment?.headline || item.evidence.anchor.text.trim();
}

const gaps: Record<string, string> = {
  catching_up: "正在分段追赶停扫期间的消息，列表可能尚不完整。",
  interrupted: "上次扫描中断，将从最近成功的位置继续。",
  identity: "本机飞书身份不可用或与配置不符，停扫期间的消息尚未补齐。",
  provider_unavailable: "飞书来源暂时不可读，停扫期间的消息尚未补齐。",
  incomplete_search: "飞书未返回完整搜索结果，存在未补齐的来源缺口。",
  storage: "扫描结果尚未保存，将从原检查点重试。",
};

function ScanStatus({ scan }: { scan: FeishuFeed["scan"] }) {
  if (scan.phase === "disabled") return <span>此 Server 尚未配置飞书采集</span>;
  return (
    <>
      <span>
        {scan.last_success_at
          ? `上次成功扫描 ${stamp(scan.last_success_at)}`
          : "尚无成功扫描"}
      </span>
      {scan.phase === "running" && <span>正在扫描</span>}
      {scan.gap && (
        <span className="fs-gap" role="status">
          {gaps[scan.gap] || "来源仍有未确认的缺口。"}
        </span>
      )}
    </>
  );
}

function matchesId(card: FeishuCard, id: string | null) {
  return id !== null && (card.evidence.id === id || card.aliases.includes(id));
}

export function FeishuSuggestions({
  selected,
  onReachable,
}: {
  selected: string | null;
  onReachable: (online: boolean) => void;
}) {
  const [data, setData] = useState<FeishuFeed | null>(null);
  const [extra, setExtra] = useState<{
    cards: FeishuCard[];
    next: string | null;
  } | null>(null);
  const [outside, setOutside] = useState<{
    requested: string;
    card: FeishuCard;
  } | null>(null);
  const [tab, setTab] = useState<Tab>("open");
  const [error, setError] = useState("");
  const [selectedError, setSelectedError] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [notice, setNotice] = useState("");
  const [reload, setReload] = useState(0);
  const cards = [...(data?.suggestions ?? [])];
  for (const card of extra?.cards ?? []) {
    if (!cards.some((item) => item.evidence.id === card.evidence.id))
      cards.push(card);
  }
  // A paginated card is only cached, so a fresh detail may resolve its
  // ID to an uncached canonical card. Let that response retire the stale row.
  const freshDetail = outside?.requested === selected ? outside?.card : null;
  const superseded = new Set([
    ...cards.flatMap((card) => card.aliases),
    ...(freshDetail?.aliases ?? []),
  ]);
  const visible = cards.filter((card) => !superseded.has(card.evidence.id));
  const filtered = visible.filter((card) => tabOf(card) === tab);
  const selectedCard =
    freshDetail ?? visible.find((card) => matchesId(card, selected)) ?? null;
  const active = selected ? selectedCard : (filtered[0] ?? null);
  const next = extra ? extra.next : data?.next_before;

  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function load() {
      try {
        const feed = await feishuFeed(controller.signal);
        if (controller.signal.aborted) return;
        setData(feed);
        setError("");
        onReachable(true);
        if (
          selected &&
          !feed.suggestions.some((item) => matchesId(item, selected))
        ) {
          try {
            const card = await feishuCard(selected, controller.signal);
            if (controller.signal.aborted) return;
            setOutside({ requested: selected, card });
            setExtra((page) => {
              if (
                !page?.cards.some((item) =>
                  card.aliases.includes(item.evidence.id),
                )
              )
                return page;
              return {
                ...page,
                cards: [
                  ...page.cards.filter(
                    (item) =>
                      item.evidence.id !== card.evidence.id &&
                      !card.aliases.includes(item.evidence.id),
                  ),
                  card,
                ],
              };
            });
            setSelectedError("");
          } catch (error) {
            if (controller.signal.aborted) return;
            setOutside(null);
            setSelectedError(errorText(error));
          }
        } else {
          setOutside(null);
          setSelectedError("");
        }
      } catch (error) {
        if (controller.signal.aborted) return;
        setError(errorText(error));
        onReachable(false);
      }
      if (!controller.signal.aborted)
        timer = setTimeout(() => void load(), 4500);
    }
    void load();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [selected, reload, onReachable]);

  async function more() {
    if (!next || loadingMore) return;
    setLoadingMore(true);
    try {
      const page = await feishuFeed(undefined, next);
      setExtra((old) => ({
        cards: [...(old?.cards ?? []), ...page.suggestions],
        next: page.next_before,
      }));
    } catch (error) {
      setNotice(errorText(error));
    } finally {
      setLoadingMore(false);
    }
  }

  async function refresh() {
    setRefreshing(true);
    setNotice("");
    try {
      await feishuSync();
      setNotice("已请求 Server 扫描。");
      setReload((value) => value + 1);
    } catch (error) {
      setNotice(errorText(error));
    } finally {
      setRefreshing(false);
    }
  }

  return (
    <main className="fs-page">
      <header className="fs-intro">
        <div>
          <p className="fs-eyebrow">
            <span aria-hidden="true" /> 飞书收件
          </p>
          <h2>值得跟进的飞书消息</h2>
          <p>只收集明确 @ 你的消息；同一话题有新进展时更新，不打扰主 Chat。</p>
        </div>
        <div className="fs-scan">
          <div>{data && <ScanStatus scan={data.scan} />}</div>
          {data && data.scan.phase !== "disabled" && (
            <button
              type="button"
              className="fs-button"
              onClick={() => void refresh()}
              disabled={refreshing}
            >
              扫描新消息
            </button>
          )}
        </div>
      </header>
      {notice && (
        <p role="status" className="fs-inline">
          {notice}
        </p>
      )}
      {error && (
        <p role="alert" className="fs-inline fs-gap">
          {error}
        </p>
      )}
      <div className="fs-layout" data-detail={!!selected}>
        <section className="fs-feed" aria-label="建议列表">
          <div className="fs-tabs" aria-label="建议状态">
            {tabs.map(([key, label]) => (
              <button
                type="button"
                key={key}
                aria-pressed={tab === key}
                onClick={() => {
                  setTab(key);
                  if (selected) navigate(base);
                }}
              >
                {label}
              </button>
            ))}
          </div>
          {filtered.map((card) => {
            const ev = card.evidence;
            return (
              <a
                className="fs-row"
                key={ev.id}
                href={detailLink(ev.id)}
                aria-current={
                  ev.id === active?.evidence.id ? "true" : undefined
                }
              >
                <span className="fs-row-meta">
                  <span>{ev.chat_name || "飞书会话"}</span>
                  <time
                    dateTime={new Date(card.updated_at * 1000).toISOString()}
                  >
                    {stamp(card.updated_at)}
                  </time>
                </span>
                <strong>{titleOf(card)}</strong>
                <span className="fs-row-foot">
                  {ev.replies.length > 0
                    ? `${ev.replies.length} 条后续回复`
                    : `由 ${ev.anchor.sender} 提及`}
                  {card.discussions.some((item) => item.attached) && (
                    <span>讨论已关联</span>
                  )}
                </span>
              </a>
            );
          })}
          {filtered.length === 0 && (
            <p className="fs-empty">
              {data
                ? data.scan.phase === "disabled"
                  ? "启用本机飞书采集后，建议会出现在这里。"
                  : "当前没有此状态的建议。"
                : "正在读取飞书建议…"}
            </p>
          )}
          {next && (
            <button
              className="fs-button fs-more"
              type="button"
              disabled={loadingMore}
              onClick={() => void more()}
            >
              更多建议
            </button>
          )}
        </section>
        <section className="fs-panel" aria-label="建议详情">
          <a className="fs-back" href={base}>
            ← 返回建议列表
          </a>
          {active ? (
            <SuggestionDetail
              key={active.evidence.id}
              card={active}
              assessor={!!data?.assessor}
              onChange={() => {
                setExtra(null);
                setReload((value) => value + 1);
              }}
            />
          ) : (
            <p className="fs-empty">
              {selected
                ? selectedError || "正在读取这条建议…"
                : "选择一条建议，查看来源和后续回复。"}
            </p>
          )}
        </section>
      </div>
    </main>
  );
}

function SourceRow({ message }: { message: FeishuMessage }) {
  return (
    <li className="fs-message">
      <div>
        <strong>{message.sender}</strong>
        <time dateTime={new Date(message.created_at * 1000).toISOString()}>
          {stamp(message.created_at)}
        </time>
      </div>
      <p>{message.text}</p>
      <a href={message.href} target="_blank" rel="noreferrer">
        飞书原文 ↗
      </a>
    </li>
  );
}

function SuggestionDetail({
  card,
  assessor,
  onChange,
}: {
  card: FeishuCard;
  assessor: boolean;
  onChange: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [choosing, setChoosing] = useState(false);
  const [choices, setChoices] = useState<Discussion[] | null>(null);
  const [target, setTarget] = useState("");
  const ev = card.evidence;
  const { id, revision, anchor, replies, assessment } = ev;
  const handoff = card.handoffs.find(
    (item) => item.suggestion_id === id && item.revision === revision,
  )?.handoff.request;
  const remaining = card.handoffs.filter(
    (item) =>
      (item.suggestion_id !== id || item.revision !== revision) &&
      item.handoff.request,
  );

  async function act(action: () => Promise<unknown>) {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (error) {
      setError(errorText(error));
    } finally {
      setBusy(false);
      onChange();
    }
  }

  async function showChoices() {
    if (choosing) {
      setChoosing(false);
      return;
    }
    setChoosing(true);
    try {
      const signal = new AbortController().signal;
      const lists = await Promise.all([
        listDiscussions(false, signal),
        listDiscussions(true, signal),
      ]);
      setChoices(lists.flatMap(({ discussions }) => discussions));
    } catch (error) {
      setError(errorText(error));
    }
  }

  async function discuss(chat?: string) {
    await act(async () => {
      const stayed = trackPageVisit();
      try {
        const result = await feishuDiscuss(id, revision, chat);
        notifyDiscussionChanged(result.chat_id);
        if (stayed()) navigate(discussionLink(result.chat_id));
      } finally {
        stayed();
      }
    });
  }

  return (
    <article className="fs-detail">
      <div className="fs-detail-meta">
        <span className="fs-pill">明确 @ 我</span>
        <span>{ev.chat_name || "飞书会话"}</span>
        {card.ignored && <span className="fs-pill neutral">已忽略</span>}
        {assessment?.disposition === "resolved" && (
          <span className="fs-pill neutral">已解决</span>
        )}
      </div>
      <h3>{titleOf(card)}</h3>
      {assessment ? (
        <>
          <p className="fs-overview">{assessment.overview}</p>
          {!!assessment.open_questions.length && (
            <div className="fs-questions">
              <h4>尚待确认</h4>
              <ul>
                {assessment.open_questions.map((question, index) => (
                  <li key={index}>{question}</li>
                ))}
              </ul>
            </div>
          )}
        </>
      ) : (
        <p className="fs-overview">
          {!assessor
            ? "当前仅展示来源材料。"
            : card.assessment_status === "failed"
              ? "研判暂不可用，可先查看来源材料。"
              : "正在等待研判，可先查看来源材料。"}
        </p>
      )}
      <div className="fs-actions">
        {handoff?.message_id ? (
          <a
            className="fs-button primary"
            href={`/#message-${handoff.message_id}`}
          >
            查看主 Chat 转交 ↗
          </a>
        ) : (
          <button
            type="button"
            className="fs-button primary"
            disabled={busy || handoff?.status === "queued"}
            onClick={() =>
              void act(() =>
                feishuHandoff(
                  id,
                  revision,
                  handoff?.status === "failed" ? handoff.generation : undefined,
                ),
              )
            }
          >
            {handoff?.status === "queued"
              ? "正在生成转交…"
              : handoff?.status === "failed"
                ? "重试转交"
                : "转交主 Chat"}
          </button>
        )}
        <button
          type="button"
          className="fs-button"
          disabled={busy}
          aria-expanded={choosing}
          onClick={() => void showChoices()}
        >
          带入讨论
        </button>
        <button
          type="button"
          className="fs-tertiary"
          disabled={busy}
          onClick={() =>
            void act(() => feishuIgnore(id, revision, !card.ignored))
          }
        >
          {card.ignored ? "撤销忽略" : "忽略"}
        </button>
      </div>
      {choosing && (
        <div className="fs-picker">
          <p>
            只附上飞书参考，不发送消息。补充想法后，可以从讨论转交最终事项。
          </p>
          <button
            type="button"
            className="fs-button"
            disabled={busy}
            onClick={() => void discuss()}
          >
            {card.discussions.some((item) => item.attached)
              ? "新建或继续此建议的讨论"
              : "新建讨论"}
          </button>
          <label>
            选择已有讨论
            <select
              value={target}
              onChange={(event) => setTarget(event.target.value)}
              disabled={!choices?.length || busy}
            >
              <option value="">
                {choices
                  ? choices.length
                    ? "请选择"
                    : "暂无已有讨论"
                  : "正在读取…"}
              </option>
              {choices?.map((choice) => (
                <option value={choice.chat_id} key={choice.chat_id}>
                  {choice.title}
                  {choice.archived ? "（已归档）" : ""}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            className="fs-button"
            disabled={!target || busy}
            onClick={() => void discuss(target)}
          >
            附到所选讨论
          </button>
        </div>
      )}
      {error && (
        <p role="alert" className="fs-gap">
          {error}
        </p>
      )}
      {handoff?.status === "failed" && (
        <p className="fs-gap">转交未送达。{handoff.error}</p>
      )}
      {(card.discussions.length > 0 || remaining.length > 0) && (
        <div className="fs-links">
          {card.discussions.map((discussion) => (
            <a
              key={`${discussion.suggestion_id}:${discussion.chat_id}`}
              href={discussionLink(discussion.chat_id)}
            >
              {discussion.attached ? "继续讨论" : "历史讨论"} ·{" "}
              {discussion.title}
              {discussion.suggestion_id !== id
                ? "（合并前材料）"
                : discussion.revision < revision
                  ? "（旧版材料）"
                  : ""}
            </a>
          ))}
          {remaining.map(
            ({
              suggestion_id: oldId,
              revision: oldRevision,
              handoff: oldHandoff,
            }) => {
              const result = oldHandoff.request;
              const key = `${oldId}:${oldRevision}`;
              const label =
                oldId === id ? `第 ${oldRevision} 版` : "合并前材料";
              if (result?.message_id) {
                return (
                  <a key={key} href={`/#message-${result.message_id}`}>
                    查看{label}的主 Chat 转交 ↗
                  </a>
                );
              }
              return (
                <div key={key} className="fs-old-handoff">
                  {result?.status === "failed" ? (
                    <>
                      <span>{label}转交未送达</span>
                      <button
                        className="fs-button"
                        type="button"
                        disabled={busy}
                        onClick={() =>
                          void act(() =>
                            feishuHandoff(
                              oldId,
                              oldRevision,
                              result.generation,
                            ),
                          )
                        }
                      >
                        重试旧版转交
                      </button>
                    </>
                  ) : (
                    <span>正在生成{label}的转交</span>
                  )}
                </div>
              );
            },
          )}
        </div>
      )}
      <section className="fs-sources" aria-label="来源与后续">
        <h4>
          来源与后续{" "}
          <span>
            {
              [
                anchor,
                ...ev.mentions.filter((m) => m.id !== anchor.id),
                ...replies,
              ].length
            }{" "}
            条材料
          </span>
        </h4>
        {ev.context_status === "pending" && (
          <p className="fs-coverage">尚未读取后续回复。</p>
        )}
        {ev.context_status === "partial" && (
          <p className="fs-coverage">回复仅部分可读，不能据此认定没有后续。</p>
        )}
        {ev.context_status === "unavailable" && (
          <p className="fs-coverage">暂时无法核实后续回复。</p>
        )}
        <ul>
          <SourceRow message={anchor} />
          {[...ev.mentions.filter((m) => m.id !== anchor.id), ...replies]
            .sort((a, b) => a.created_at - b.created_at)
            .map((message) => (
              <SourceRow key={message.id} message={message} />
            ))}
        </ul>
      </section>
    </article>
  );
}
