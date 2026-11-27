import type { QuickHandoff } from "./discussionApi";
import { sessionFetch } from "./webSession";

export type FeishuMessage = {
  id: string;
  sender: string;
  text: string;
  created_at: number;
  href: string;
};

export type FeishuCard = {
  evidence: {
    id: string;
    revision: number;
    chat_name: string;
    anchor: FeishuMessage;
    mentions: FeishuMessage[];
    replies: FeishuMessage[];
    context_status: "pending" | "ready" | "partial" | "unavailable";
    assessment: {
      headline: string;
      overview: string;
      open_questions: string[];
      evidence_ids: string[];
      disposition: "open" | "resolved" | "unclear";
    } | null;
  };
  updated_at: number;
  ignored: boolean;
  assessment_status: "pending" | "assessed" | "failed";
  discussions: {
    chat_id: string;
    title: string;
    revision: number;
    attached: boolean;
    archived: boolean;
  }[];
  handoffs: { revision: number; handoff: QuickHandoff }[];
};

export type FeishuFeed = {
  scan: {
    phase:
      "disabled" | "starting" | "idle" | "running" | "interrupted" | "failed";
    last_success_at?: number | null;
    checkpoint_at?: number | null;
    gap?: string | null;
  };
  assessor: { id: string; category: "feishu-router" } | null;
  suggestions: FeishuCard[];
  next_before: string | null;
};

const root = "/api/v1/feishu-suggestions";

async function request<T>(
  path = "",
  { signal, body }: { signal?: AbortSignal; body?: unknown } = {},
): Promise<T> {
  const response = await sessionFetch(root + path, {
    signal,
    method: body === undefined ? "GET" : "POST",
    headers:
      body === undefined ? undefined : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    const result = (await response.json().catch(() => ({}))) as {
      error?: string;
    };
    if (response.status === 409)
      throw new Error("建议或扫描状态已变化，请刷新后再试。");
    if (response.status === 404)
      throw new Error("建议已过期，或对应讨论已不存在。");
    throw new Error(result.error || "暂时无法读取飞书建议。");
  }
  return (await response.json()) as T;
}

export const feishuFeed = (signal?: AbortSignal, before?: string) =>
  request<FeishuFeed>(before ? `?before=${encodeURIComponent(before)}` : "", {
    signal,
  });
export const feishuCard = (id: string, signal: AbortSignal) =>
  request<FeishuCard>(`/${encodeURIComponent(id)}`, { signal });
export const feishuSync = () =>
  request<{ requested: true }>("/sync", { body: {} });
export const feishuIgnore = (id: string, revision: number, ignored: boolean) =>
  request(`/${id}/ignore`, { body: { revision, ignored } });
export const feishuDiscuss = (id: string, revision: number, chat_id?: string) =>
  request<{ chat_id: string }>(`/${id}/discussions`, {
    body: { revision, chat_id },
  });
export const feishuHandoff = (
  id: string,
  revision: number,
  retry_generation?: number,
) =>
  request<QuickHandoff>(`/${id}/handoff`, {
    body: { revision, retry_generation },
  });
