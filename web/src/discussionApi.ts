import { sessionFetch } from "./webSession";

export const discussionChanged = "tend-discussion-changed";

export function notifyDiscussionChanged(chat?: string) {
  window.dispatchEvent(new CustomEvent(discussionChanged, { detail: chat }));
}

export type Source =
  | { kind: "repo"; repo_id: string }
  | { kind: "issue"; issue_id: string }
  | { kind: "pull"; url: string }
  | { kind: "worktree"; issue_id: string; task_id: string; repo_id: string };
export type Reference = Source & {
  id: string;
  label: string;
  href: string | null;
  status: string;
};
export type Handoff = { message_id: string; draft_id: string; href: string };
export type Discussion = {
  chat_id: string;
  title: string;
  updated_at: number;
  archived: boolean;
};
export type DiscussionDetail = Discussion & {
  origin: { label: string; href: string } | null;
  references: Reference[];
  handoffs: Handoff[];
  execution_configured: boolean;
};
export type IssueChoice = {
  issue_id: string;
  title: string;
  short_number: number;
};
export type WorkspaceChoice = IssueChoice & {
  task_id: string;
  repo_id: string;
  status: string;
};
export type Draft = {
  goal: string;
  decisions: string;
  constraints: string;
  authorization_end: string;
};
export type DraftRequest = {
  id: string;
  status: "queued" | "failed" | "completed";
  error: string | null;
  draft: Draft | null;
  message_id: string | null;
};
export type SendDraft = Draft & {
  operation_id: string;
  draft_id: string;
  reference_ids: string[];
  target_issue_id?: string;
};

const root = "/api/v1/discussions";
export const discussionLink = (chat: string) =>
  `/discussions?chat=${encodeURIComponent(chat)}`;
export const discussionIndexLink = (archived = false) =>
  archived ? "/discussions?archived=true" : "/discussions";

export class DiscussionError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

async function request<T>(
  path = "",
  options: { method?: string; body?: unknown; signal?: AbortSignal } = {},
): Promise<T> {
  const response = await sessionFetch(root + path, {
    method: options.method,
    signal: options.signal,
    headers: options.body ? { "Content-Type": "application/json" } : undefined,
    body: options.body ? JSON.stringify(options.body) : undefined,
  });
  if (!response.ok) {
    const body = (await response.json().catch(() => ({}))) as {
      error?: string;
    };
    throw new DiscussionError(
      response.status,
      body.error ?? "暂时无法完成讨论请求",
    );
  }
  return response.json() as Promise<T>;
}

export const listDiscussions = (archived: boolean, signal: AbortSignal) =>
  request<{ discussions: Discussion[] }>(archived ? "?archived=true" : "", {
    signal,
  });
export const createDiscussion = (operation_id: string) =>
  request<{ chat_id: string }>("", {
    method: "POST",
    body: { operation_id },
  });
export const readDiscussion = (chat: string, signal: AbortSignal) =>
  request<DiscussionDetail>(`/${chat}`, { signal });
export const renameDiscussion = (chat: string, title: string) =>
  request(`/${chat}`, { method: "PATCH", body: { title } });
export const setDiscussionArchived = (chat: string, archived: boolean) =>
  request<{ chat_id: string; archived: boolean }>(`/${chat}`, {
    method: "PATCH",
    body: { archived },
  });
export const attachReference = (chat: string, source: Source) =>
  request<Reference>(`/${chat}/references`, { method: "POST", body: source });
export const detachReference = (chat: string, id: string) =>
  request(`/${chat}/references?id=${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
export const findReferences = (
  query: string,
  target: boolean,
  signal: AbortSignal,
) =>
  request<{ repos: string[]; issues: IssueChoice[] }>(
    `/candidates?${new URLSearchParams({ q: query, target: String(target) })}`,
    { signal },
  );
export const readWorkspaceChoices = (issue: string, signal: AbortSignal) =>
  request<{ choices: WorkspaceChoice[] }>(
    `/workspaces?issue=${encodeURIComponent(issue)}`,
    { signal },
  );
export const readDraft = (chat: string, signal: AbortSignal, id?: string) =>
  request<{ draft_request: DraftRequest | null }>(
    `/${chat}/draft${id ? `?id=${encodeURIComponent(id)}` : ""}`,
    { signal },
  );
export const requestDraft = (chat: string, operation_id: string) =>
  request<{ id: string }>(`/${chat}/draft`, {
    method: "POST",
    body: { operation_id },
  });
export const sendDraft = (chat: string, body: SendDraft) =>
  request<{ message_id: string }>(`/${chat}/handoffs`, {
    method: "POST",
    body,
  });

export const referenceStatus: Record<string, string> = {
  available: "可读",
  repo_not_readable_here: "此仓库在本机不可读",
  different_host: "不在 Server 所在主机",
  server_host_not_configured: "Server 未配置讨论主机",
  runner_inspection_not_configured: "Runner 未配置查询通道",
  runner_workspace_query_unsupported: "此 Runner 不支持查询 worktree",
  runner_unavailable: "Runner 暂不可访问",
  runner_response_invalid: "无法核实 Runner 返回的目录身份",
  workspace_authority_changed: "Task 的 Runner 归属已变化，请刷新",
  workspace_not_assigned: "Task 尚未由 Runner 领取",
  workspace_not_recorded: "Task 未记录可验证的工作目录",
  workspace_reclaimed_or_changed: "工作目录已回收或发生变化",
  no_registered_mount: "Task 没有此仓库的已登记挂载",
  check_on_selection: "选择后核实实际挂载",
  provider_not_configured: "此 PR/MR 来源尚未配置读取能力",
  github_cli_unavailable: "未配置可用的 GitHub 读取程序",
  not_accessible: "未获读取权限或该 PR 不存在",
  github_query_timeout: "GitHub 读取超时",
  github_query_failed: "GitHub 读取失败，请检查认证与网络",
  invalid_response: "未能确认来源返回的信息",
};
