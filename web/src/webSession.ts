const path = "/api/v1/web/session";
export const logoutSignal = "tend.web.logout.v1";
const legacyToken = "tend.token.v1";

type Active = { controller: AbortController; onUnauthorized: () => void };
let active: Active | undefined;

export function beginSession(onUnauthorized: () => void) {
  const scope = { controller: new AbortController(), onUnauthorized };
  active = scope;
  return () => {
    if (active === scope) active = undefined;
    scope.controller.abort();
  };
}

function sameOriginFetch(url: string, options?: RequestInit) {
  const headers = new Headers(options?.headers);
  if (options?.method && !["GET", "HEAD"].includes(options.method)) {
    headers.set("X-Tend-Web", "1");
  }
  return fetch(url, {
    ...options,
    headers,
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
  });
}

export async function sessionFetch(url: string, options?: RequestInit) {
  const scope = active;
  if (!scope) throw new DOMException("Tend Web session ended", "AbortError");
  const lifecycle = scope.controller.signal;
  const signal = options?.signal
    ? AbortSignal.any([options.signal, lifecycle])
    : lifecycle;
  const response = await sameOriginFetch(url, { ...options, signal });
  // A late response from an old request must not tear down a newer login.
  lifecycle.throwIfAborted();
  if (response.status === 401) {
    if (active === scope) scope.onUnauthorized();
    throw new Error("登录已失效，请重新连接 Tend。");
  }
  return response;
}

export async function readSession(signal?: AbortSignal) {
  const response = await sameOriginFetch(path, { signal });
  signal?.throwIfAborted();
  if (response.status === 401) return false;
  if (response.status !== 204) throw new Error("暂时无法检查 Tend 登录状态");
  return true;
}

function writeSession<T>(operation: () => Promise<T>, signal?: AbortSignal) {
  // When available, serialize Cookie-changing responses across same-origin tabs.
  return (
    navigator.locks?.request("tend.web.session", { signal }, operation) ??
    operation()
  );
}

export function allowsWebLogin({
  protocol,
  hostname,
}: Pick<Location, "protocol" | "hostname">) {
  return (
    protocol === "https:" ||
    (protocol === "http:" &&
      ["localhost", "127.0.0.1", "[::1]"].includes(hostname))
  );
}

export async function loginSession(
  token: string,
  remember: boolean,
  signal?: AbortSignal,
) {
  if (!allowsWebLogin(location))
    throw new Error("请打开已配置的 HTTPS 地址后登录 Tend。");

  return writeSession(async () => {
    signal?.throwIfAborted();
    const response = await sameOriginFetch(path, {
      method: "POST",
      signal,
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ remember }),
    });
    signal?.throwIfAborted();
    if (response.status === 401) throw new Error("访问令牌无效");
    if (response.status === 403)
      throw new Error("请使用已配置的 HTTPS 地址，或从本机回环地址连接。");
    if (response.status !== 204) throw new Error("暂时无法连接 Tend，请重试。");
    if (!(await readSession(signal)))
      throw new Error("浏览器未保存会话，请允许此站点使用 Cookie 后重试。");
    signal?.throwIfAborted();

    try {
      sessionStorage.removeItem(legacyToken);
      return "";
    } catch {
      return "当前浏览器限制了站点存储，无法清除旧登录数据。可在浏览器中清除此站点数据后重新连接。";
    }
  }, signal);
}

export function logoutSession() {
  return writeSession(async () => {
    const response = await sameOriginFetch(path, { method: "DELETE" });
    if (response.status !== 204 && response.status !== 401)
      throw new Error("暂时无法断开连接");
  });
}
