export type Artifact = {
  id: string;
  name: string;
  bytes: number;
  available: boolean;
};

export type Message = {
  id: string;
  text: string;
  author: "user" | "router";
  reply_to: string | null;
  router_state: "queued" | "awaiting_turn" | "completed" | "failed" | null;
  router_error: string | null;
  artifacts?: Artifact[];
  attention?: {
    id: string;
    state: "pending" | "awaiting_user" | "answered" | "delivered";
  };
};

export type Page = { messages: Message[]; next_before: number | null };
export type PendingMessage = {
  operation_id: string;
  text: string;
  reply_to?: string;
};

const messages = "/api/v1/chats/main/messages";

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
) {
  const path = before === undefined ? messages : `${messages}?before=${before}`;
  return (await request(token, path, { signal })).json() as Promise<Page>;
}

export async function sendMessage(token: string, message: PendingMessage) {
  await request(token, messages, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(message),
  });
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
