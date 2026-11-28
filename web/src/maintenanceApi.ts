import { sessionFetch } from "./webSession";

export type MaintenanceView = "active" | "history" | "ignored";
type Link = { label: string; href: string };

export type Suggestion = {
  id: string;
  kind: "feishu_scan_stalled" | "task_execution_failed";
  status: "active" | "not_observed" | "recovered";
  first_seen: number;
  last_seen: number;
  changed_at: number;
  revision: number;
  ignored: boolean;
  fault: {
    title: string;
    impact: string;
    known: string;
    unknown: string;
    links: Link[];
  };
  followup: { known: string; link: Link | null } | null;
};

export type MaintenancePage = {
  suggestions: Suggestion[];
  next_before: string | null;
  feishu_configured: boolean;
};
export type MaintenanceDetail = { suggestion: Suggestion; note: string };
export type ShareReceipt = {
  chat_id: string;
  message_id: string;
  href: string;
};

export class MaintenanceRejection extends Error {}

async function request<T>(path: string, signal?: AbortSignal, body?: unknown) {
  const response = await sessionFetch(
    `/api/v1/maintenance-suggestions${path}`,
    {
      method: body === undefined ? "GET" : "POST",
      headers:
        body === undefined ? undefined : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal,
    },
  );
  if (!response.ok) {
    if (response.status === 409)
      throw new MaintenanceRejection("证据版本已更新，请查看新证据后再试。");
    if (response.status === 404)
      throw new MaintenanceRejection("找不到此建议，或所选讨论已归档。");
    throw new Error("暂时无法处理维护建议。");
  }
  return (await response.json()) as T;
}

export const listMaintenance = (
  view: MaintenanceView,
  before: string | null,
  signal: AbortSignal,
) => {
  const query = new URLSearchParams({ view });
  if (before) query.set("before", before);
  return request<MaintenancePage>(`?${query}`, signal);
};
export const readMaintenance = (id: string, signal: AbortSignal) =>
  request<MaintenanceDetail>(`/${encodeURIComponent(id)}`, signal);
export const ignoreMaintenance = (s: Suggestion) =>
  request(`/${s.id}/ignore`, undefined, {
    ignored: !s.ignored,
    revision: s.revision,
  });

async function share(path: string, body: unknown): Promise<ShareReceipt> {
  const receipt = await request<Partial<ShareReceipt> | null>(
    path,
    undefined,
    body,
  );
  if (
    typeof receipt?.chat_id !== "string" ||
    typeof receipt.message_id !== "string" ||
    typeof receipt.href !== "string"
  )
    throw new Error("未收到完整的分享回执。");
  return {
    chat_id: receipt.chat_id,
    message_id: receipt.message_id,
    href: receipt.href,
  };
}

export const discussMaintenance = (s: Suggestion, chat?: string) =>
  share(`/${s.id}/discussions`, {
    chat_id: chat,
    revision: s.revision,
  });
export const handoffMaintenance = (s: Suggestion) =>
  share(`/${s.id}/handoff`, {
    revision: s.revision,
  });
