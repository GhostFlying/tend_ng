import { sessionFetch } from "./webSession";

export type Artifact = {
  id: string;
  name: string;
  bytes: number;
  available: boolean;
};

export type AgentProfile = {
  id: string;
  category: string;
  revision: string;
  model: string | null;
  reasoning_effort: string | null;
  app_server?: string;
  description?: string;
  capabilities: string[];
};

export type RunnerInstance = {
  profile: {
    host_id: string;
    runner_id: string;
    agents: AgentProfile[];
    repos: string[];
    running_slots: number;
    slot_limit: number;
    available_for_assignment: number;
    standby?: boolean;
  };
  protocol: number;
  online: boolean;
  last_seen: number;
  successor: string | null;
  drained_at: number | null;
  retired_at: number | null;
};

export type AgentDirectory = {
  router: Pick<AgentProfile, "id" | "model" | "reasoning_effort">;
  runners: RunnerInstance[];
  activity: {
    runner_id: string;
    agent_id: string;
    revision: string;
    running: number;
  }[];
};

export type PullLink = {
  url: string;
  provider: "github" | "codebase";
  repository: string;
  number: number;
  facts: { state: string; draft: boolean; ci: string | null } | null;
  observed_at: number | null;
  observation_error: string | null;
};

export type BoardStage =
  "backlog" | "todo" | "in_progress" | "waiting" | "done" | "cancelled";

export const issueStageLabels: Record<BoardStage, string> = {
  backlog: "待安排",
  todo: "待推进",
  in_progress: "进行中",
  waiting: "等待中",
  done: "已完成",
  cancelled: "已取消",
};

export type BoardIssue = {
  id: string;
  short_number?: number;
  parent_issue_id: string | null;
  title: string;
  issue_state: string;
  task_state: string;
  stage: BoardStage;
  running: number;
  repo_ids?: string[];
  waiting_for?: "external" | "reply" | "children" | "continuation";
  attention?: { chat_id: string; question_message_id: string };
  pulls: PullLink[];
};

export type BoardPage = { issues: BoardIssue[]; next_before: string | null };

export type IssueReference = {
  id: string;
  short_number?: number;
  title: string;
};
export type IssueDelivery = Artifact & {
  issue_title?: string;
  source?: { issue: IssueReference; task_id: string };
};
type IssueReportContent = {
  message_id: string;
  text: string;
  artifacts: IssueDelivery[];
};
export type IssueDeliveryReport = IssueReportContent & {
  report_id?: string;
  history?: (IssueReportContent & { report_id: string })[];
};

export type ApprovalDecision = {
  id: string;
  version: number;
  decided_at: number;
  continuation_run_id?: string;
  continuation_blocked?: boolean;
};

export type ProposalStatus = {
  issue_id: string;
  version: number;
  question: string;
  action: string;
  can_approve: boolean;
  decision?: ApprovalDecision;
};

export type ProposalVersion = {
  version: number;
  report_id: string;
  title: string;
  highlights: string[];
  updated_at: number;
  artifact: Artifact;
  other_artifacts: Artifact[];
  message_id: string | null;
};

export type IssueProposal = {
  attention_id: string;
  question_message_id: string | null;
  status: ProposalStatus;
  versions: ProposalVersion[];
};

export type IssueDecision = {
  message_id: string;
  text: string;
  reply?: { attention_id: string; answer: string | null };
};

type IssueAttentionReplyPage = {
  decisions: IssueDecision[];
  next_before: number | null;
};

type IssueApprovalEvent = {
  report_title: string;
  decision: ApprovalDecision;
  issue_id: string;
  before_message_id: string | null;
};

export type IssueOverviewData = {
  id: string;
  short_number?: number;
  title: string;
  stage: BoardStage;
  objective: string;
  repo_ids: string[];
  chat_id: string;
  source_message_id: string;
  children: IssueReference[];
  pulls: PullLink[];
  artifacts: IssueDelivery[];
  reports?: IssueDeliveryReport[];
  decisions?: IssueDecision[];
  decision_history_before?: number;
  proposals?: IssueProposal[];
  proposal_requires_chat?: boolean;
};

export type Activity = {
  seq: number;
  issue_id: string;
  short_number?: number;
  issue_title: string;
  previous: string;
  current: string;
  changed_at: number;
};

export type NotificationPage = {
  activities: Activity[];
  next_before: number | null;
};

export type Message = {
  id: string;
  text: string;
  author: "user" | "router" | "discussion";
  reply_to: string | null;
  router_state: "queued" | "awaiting_turn" | "completed" | "failed" | null;
  router_error: string | null;
  artifacts?: Artifact[];
  issue_ids?: string[];
  issue_numbers?: number[];
  issue?: IssueMessage;
  approval?: IssueApprovalEvent;
  attention?: AttentionStatus;
  discussion_id?: string;
};

export type AttentionStatus = {
  id: string;
  state: "pending" | "awaiting_user" | "answered" | "delivered";
  proposal?: ProposalStatus;
  issue_reply?: string;
};

export type IssueMessage = {
  id: string;
  issue_id: string;
  short_number?: number;
  issue_title: string;
  kind: "user" | "agent" | "dispatch" | "relay" | "run" | "takeover";
  author: string;
  text: string;
  reply_to: string | null;
  run_id: string | null;
  source_message_id: string | null;
  forwarded_from?: string;
  dispatch_provenance?:
    | { origin: "main_chat" | "issue_delegation"; agent_name?: string }
    | { origin: "other_chat" };
};

export type Page = {
  messages: Message[];
  next_before: number | null;
  title?: string;
  issue_id?: string;
  short_number?: number;
  chat_id?: string;
  awaiting_start?: boolean;
};

export function chatLink(chat: string, message?: string) {
  const path = chat === "main" ? "/" : `/?chat=${encodeURIComponent(chat)}`;
  return message ? `${path}#message-${encodeURIComponent(message)}` : path;
}

export function issueLink(id: string, number?: number) {
  return `/issues/${encodeURIComponent(number ?? id)}`;
}

export function issueTitle(title: string, number?: number) {
  return number === undefined ? title : `#${number} ${title}`;
}

function messagePath(issue?: string, chat = "main") {
  return issue
    ? `/api/v1/issues/${encodeURIComponent(issue)}/messages`
    : `/api/v1/chats/${encodeURIComponent(chat)}/messages`;
}
export type PendingMessage = {
  operation_id: string;
  text: string;
  reply_to?: string;
};

export function operationId() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(
    "",
  );
}

async function request(path: string, options?: RequestInit) {
  const response = await sessionFetch(path, options);
  if (!response.ok) {
    const body: { error?: string } = await response.json().catch(() => ({}));
    if (response.status === 409 && path.startsWith("/api/v1/artifacts/")) {
      throw new Error("文件正在同步，请稍后重试");
    }
    throw new Error(body.error ?? "Tend 无法完成请求");
  }
  return response;
}

export async function readAttentionStatus(
  chat: string,
  attention: string,
  signal: AbortSignal,
) {
  const path = `/api/v1/chats/${encodeURIComponent(chat)}/attentions/${encodeURIComponent(attention)}`;
  const response = await request(path, { signal });
  return response.json() as Promise<AttentionStatus>;
}

export async function readProposalStatus(
  chat: string,
  attention: string,
  signal: AbortSignal,
) {
  return (await readAttentionStatus(chat, attention, signal)).proposal;
}

export async function approveProposal(
  chat: string,
  attention: string,
  version: number,
  fromIssue = false,
) {
  const path = `/api/v1/chats/${encodeURIComponent(chat)}/attentions/${encodeURIComponent(attention)}/approve`;
  const response = await request(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ version, from_issue: fromIssue }),
  });
  return response.json() as Promise<ApprovalDecision>;
}

export type PendingAttentionReply = { operation_id: string; text: string };

export async function answerIssueAttention(
  issue: string,
  attention: string,
  reply: PendingAttentionReply,
) {
  const path = `/api/v1/issues/${encodeURIComponent(issue)}/attentions/${encodeURIComponent(attention)}/answer`;
  const response = await request(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(reply),
  });
  return response.json() as Promise<{
    operation_id: string;
    attention_id: string;
    answer: string;
  }>;
}

export async function readMessages(
  before?: number,
  signal?: AbortSignal,
  issue?: string,
  chat?: string,
): Promise<Page> {
  const messages = messagePath(issue, chat);
  const path = before === undefined ? messages : `${messages}?before=${before}`;
  const response = await request(path, { signal });
  if (!issue) return response.json() as Promise<Page>;
  const page: {
    title: string;
    issue_id?: string;
    short_number?: number;
    chat_id?: string;
    awaiting_start?: boolean;
    next_before: number | null;
    messages: IssueMessage[];
    approvals?: IssueApprovalEvent[];
  } = await response.json();
  const eventsBefore = new Map<string | null, Message[]>();
  for (const approval of page.approvals ?? []) {
    const key = approval.before_message_id;
    const rows = eventsBefore.get(key) ?? [];
    rows.push({
      id: `approval-${approval.decision.id}`,
      approval,
      text: "",
      author: "router",
      reply_to: null,
      router_state: null,
      router_error: null,
    });
    eventsBefore.set(key, rows);
  }
  return {
    title: page.title,
    issue_id: page.issue_id,
    short_number: page.short_number,
    chat_id: page.chat_id,
    awaiting_start: page.awaiting_start,
    next_before: page.next_before,
    messages: [
      ...page.messages.flatMap((message): Message[] => [
        ...(eventsBefore.get(message.id) ?? []),
        {
          id: message.id,
          text: message.text,
          author: message.kind === "user" ? "user" : "router",
          reply_to: message.reply_to,
          router_state: null,
          router_error: null,
          issue: message,
        },
      ]),
      ...(eventsBefore.get(null) ?? []),
    ],
  };
}

export async function readIssueBoard(
  before: string | undefined,
  signal: AbortSignal,
) {
  const path = "/api/v1/issue-board";
  const url =
    before === undefined
      ? path
      : `${path}?before=${encodeURIComponent(before)}`;
  return (await request(url, { signal })).json() as Promise<BoardPage>;
}

export async function readIssueOverview(issue: string, signal: AbortSignal) {
  const path = `/api/v1/issues/${encodeURIComponent(issue)}/overview`;
  return (await request(path, { signal })).json() as Promise<IssueOverviewData>;
}

export type TokenField = {
  tokens: string | null;
  reported_requests: number;
};

export type TokenBreakdown = {
  started_runs: number;
  observed_runs: number;
  unobserved_before_collection: number;
  unobserved_no_usage: number;
  reported_requests: number;
  total_tokens: TokenField;
  input_tokens: TokenField;
  cached_input_tokens: TokenField;
  cache_write_input_tokens: TokenField;
  output_tokens: TokenField;
  reasoning_output_tokens: TokenField;
};

export type IssueTokenUsageData = {
  direct: TokenBreakdown;
  subtree: TokenBreakdown;
  nodes: {
    issue_id: string;
    number: number;
    title: string;
    direct: TokenBreakdown;
  }[];
};

export async function readIssueTokenUsage(issue: string, signal: AbortSignal) {
  const path = `/api/v1/issues/${encodeURIComponent(issue)}/token-usage`;
  return (
    await request(path, { signal })
  ).json() as Promise<IssueTokenUsageData>;
}

export type TokenOverviewIssue = IssueTokenUsageData["nodes"][number] & {
  subtree: TokenBreakdown;
  has_children: boolean;
};

export type TokenOverviewPage = {
  total?: TokenBreakdown;
  issues: TokenOverviewIssue[];
  next_before: string | null;
};

export async function readTokenOverview(
  parent: string | undefined,
  before: string | null,
  signal: AbortSignal,
) {
  const query = new URLSearchParams();
  if (parent) query.set("parent", parent);
  if (before) query.set("before", before);
  const url = `/api/v1/token-usage${query.size ? `?${query}` : ""}`;
  return (await request(url, { signal })).json() as Promise<TokenOverviewPage>;
}

export async function readIssueAttentionHistory(
  issue: string,
  before: number,
  signal: AbortSignal,
) {
  const path = `/api/v1/issues/${encodeURIComponent(issue)}/attention-replies?before=${before}`;
  return (
    await request(path, { signal })
  ).json() as Promise<IssueAttentionReplyPage>;
}

export async function readNotifications(before?: number, signal?: AbortSignal) {
  const path = "/api/v1/chats/main/notifications";
  const url = before === undefined ? path : `${path}?before=${before}`;
  return (await request(url, { signal })).json() as Promise<NotificationPage>;
}

type ActivityIssue = {
  id: string;
  title: string;
  short_number: number | null;
};

export type RunningTask = {
  run_id: string;
  task_id: string;
  category: string;
  agent_id: string;
  started_at: number | null;
  issue: ActivityIssue;
  root_issue: ActivityIssue;
};

export type ExecutionRunner = {
  runner_id: string;
  mode: "serving" | "draining" | "standby" | "drained";
  online: boolean;
  running_slots: number | null;
  slot_limit: number;
  runs: RunningTask[];
};

export type ExecutionHost = {
  host_id: string;
  running_slots: number;
  slot_limit: number;
  runners: ExecutionRunner[];
};

export type ExecutionHostSnapshot = {
  observed_at: number;
  hosts: ExecutionHost[];
};

export async function readExecutionHosts(signal: AbortSignal) {
  return (
    await request("/api/v1/execution-hosts", { signal })
  ).json() as Promise<ExecutionHostSnapshot>;
}

export async function readAgents(signal: AbortSignal) {
  return (
    await request("/api/v1/agents", { signal })
  ).json() as Promise<AgentDirectory>;
}

export async function sendMessage(
  message: PendingMessage,
  issue?: string,
  chat?: string,
) {
  const response = await request(messagePath(issue, chat), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(message),
  });
  // Finish reading the receipt before clearing the saved retry.
  await response.json();
}

type RolloutItem = { type: string; text?: string; [field: string]: unknown };

export async function readRollout(run: string, signal: AbortSignal) {
  const response = await request(
    `/api/v1/runs/${encodeURIComponent(run)}/rollout`,
    { signal },
  );
  const data: { turn: { items: RolloutItem[] } } = await response.json();
  return data.turn.items
    .filter((item) => item.type !== "userMessage")
    .map((item) =>
      item.type === "agentMessage" && item.text
        ? item.text
        : JSON.stringify(item, null, 2),
    )
    .join("\n\n");
}

function readArtifact(id: string, signal?: AbortSignal) {
  return request(`/api/v1/artifacts/${encodeURIComponent(id)}`, {
    signal,
  });
}

export const markdownPreviewLimit = 1024 * 1024;

export async function readArtifactMarkdown(id: string, signal: AbortSignal) {
  const response = await readArtifact(id, signal);
  const reader = response.body?.getReader();
  if (!reader) throw new Error("无法读取 Markdown 原件");
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let read = 0;
  let markdown = "";
  try {
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) return markdown + decoder.decode();
      read += chunk.value.byteLength;
      if (read > markdownPreviewLimit)
        throw new Error("文件太大，请下载原件阅读");
      markdown += decoder.decode(chunk.value, { stream: true });
    }
  } finally {
    await reader.cancel();
  }
}

export async function download(file: Artifact) {
  const response = await readArtifact(file.id);
  const url = URL.createObjectURL(await response.blob());
  const link = document.createElement("a");
  link.href = url;
  link.download = file.name;
  link.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function mergeMessages(
  saved: Message[],
  page: Message[],
  earlier = false,
) {
  const messagesById = new Map<string, Message>();
  for (const message of earlier ? [...page, ...saved] : [...saved, ...page]) {
    messagesById.set(message.id, message);
  }
  return Array.from(messagesById.values());
}
