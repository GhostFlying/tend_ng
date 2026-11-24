export type Artifact = {
  id: string;
  name: string;
  bytes: number;
  available: boolean;
};

export type PullLink = {
  url: string;
  number: number;
  facts: { state: string; draft: boolean; ci: string | null } | null;
  observation_error: string | null;
};

export type BoardStage =
  "backlog" | "todo" | "in_progress" | "waiting" | "done" | "cancelled";

export type BoardIssue = {
  id: string;
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

export type BoardPage = { issues: BoardIssue[]; next_before: number | null };

export type Activity = {
  seq: number;
  issue_id: string;
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
  author: "user" | "router";
  reply_to: string | null;
  router_state: "queued" | "awaiting_turn" | "completed" | "failed" | null;
  router_error: string | null;
  artifacts?: Artifact[];
  issue_ids?: string[];
  issue?: IssueMessage;
  attention?: {
    id: string;
    state: "pending" | "awaiting_user" | "answered" | "delivered";
  };
};

type IssueMessage = {
  id: string;
  issue_id: string;
  issue_title: string;
  kind: "user" | "agent" | "dispatch" | "relay" | "run";
  author: string;
  text: string;
  reply_to: string | null;
  run_id: string | null;
  source_message_id: string | null;
  forwarded_from?: string;
};

export type Page = {
  messages: Message[];
  next_before: number | null;
  title?: string;
  chat_id?: string;
  awaiting_start?: boolean;
};

export function chatLink(chat: string, message?: string) {
  const path = chat === "main" ? "/" : `/?chat=${encodeURIComponent(chat)}`;
  return message ? `${path}#message-${encodeURIComponent(message)}` : path;
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

async function request(token: string, path: string, options?: RequestInit) {
  const headers = new Headers(options?.headers);
  headers.set("Authorization", `Bearer ${token}`);
  const response = await fetch(path, { ...options, headers });
  if (!response.ok) {
    const body: { error?: string } = await response.json().catch(() => ({}));
    if (response.status === 401) throw new Error("访问令牌无效");
    if (response.status === 409 && path.startsWith("/api/v1/artifacts/")) {
      throw new Error("文件正在同步，请稍后重试");
    }
    throw new Error(body.error ?? "Tend 无法完成请求");
  }
  return response;
}

export async function readMessages(
  token: string,
  before?: number,
  signal?: AbortSignal,
  issue?: string,
  chat?: string,
): Promise<Page> {
  const messages = messagePath(issue, chat);
  const path = before === undefined ? messages : `${messages}?before=${before}`;
  const response = await request(token, path, { signal });
  if (!issue) return response.json() as Promise<Page>;
  const page: {
    title: string;
    chat_id?: string;
    awaiting_start?: boolean;
    next_before: number | null;
    messages: IssueMessage[];
  } = await response.json();
  return {
    title: page.title,
    chat_id: page.chat_id,
    awaiting_start: page.awaiting_start,
    next_before: page.next_before,
    messages: page.messages.map((message) => ({
      id: message.id,
      text: message.text,
      author: message.kind === "user" ? "user" : "router",
      reply_to: message.reply_to,
      router_state: null,
      router_error: null,
      issue: message,
    })),
  };
}

export async function readIssueBoard(
  token: string,
  before: number | undefined,
  signal: AbortSignal,
) {
  const path = "/api/v1/issue-board";
  const url = before === undefined ? path : `${path}?before=${before}`;
  return (await request(token, url, { signal })).json() as Promise<BoardPage>;
}

export async function readNotifications(
  token: string,
  before?: number,
  signal?: AbortSignal,
) {
  const path = "/api/v1/chats/main/notifications";
  const url = before === undefined ? path : `${path}?before=${before}`;
  return (
    await request(token, url, { signal })
  ).json() as Promise<NotificationPage>;
}

export async function sendMessage(
  token: string,
  message: PendingMessage,
  issue?: string,
  chat?: string,
) {
  await request(token, messagePath(issue, chat), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(message),
  });
}

type RolloutItem = { type: string; text?: string; [field: string]: unknown };

export async function readRollout(
  token: string,
  run: string,
  signal: AbortSignal,
) {
  const response = await request(
    token,
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

export async function download(token: string, file: Artifact) {
  const response = await request(
    token,
    `/api/v1/artifacts/${encodeURIComponent(file.id)}`,
  );
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
