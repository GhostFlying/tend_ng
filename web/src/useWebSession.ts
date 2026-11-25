import { useCallback, useEffect, useRef, useState } from "react";
import {
  beginSession,
  loginSession,
  logoutSession,
  logoutSignal,
  readSession,
} from "./webSession";

const expired = "登录已失效，请重新连接 Tend。";

export function useWebSession() {
  const [state, setState] = useState<"checking" | "connected" | "signed-out">(
    "checking",
  );
  const [notice, setNotice] = useState("");
  const [ending, setEnding] = useState(false);
  const stop = useRef<(() => void) | undefined>(undefined);
  const channel = useRef<BroadcastChannel | undefined>(undefined);
  const revision = useRef(0);
  const signingIn = useRef<AbortController | undefined>(undefined);
  const logoutInFlight = useRef(false);

  const signedOut = useCallback((message: string) => {
    revision.current++;
    stop.current?.();
    stop.current = undefined;
    setState("signed-out");
    setNotice(message);
  }, []);

  const signedIn = useCallback(
    (message: string) => {
      revision.current++;
      stop.current?.();
      stop.current = beginSession(() => signedOut(expired));
      setState("connected");
      setNotice(message);
    },
    [signedOut],
  );

  useEffect(() => {
    let starting = true;
    let probe: AbortController | undefined;

    async function check(replace = false) {
      if ((!starting && !stop.current) || (probe && !replace)) return;
      // A logout invalidates an outstanding observation, even during startup.
      probe?.abort();
      const controller = new AbortController();
      probe = controller;
      const before = revision.current;
      const isCurrent = () =>
        !controller.signal.aborted && before === revision.current;
      try {
        const valid = await readSession(controller.signal);
        if (!isCurrent()) return;
        if (!valid) signedOut(starting ? "" : expired);
        else if (starting) signedIn("");
      } catch {
        if (starting && isCurrent())
          signedOut("暂时无法检查登录状态。请检查网络后刷新或重新连接。");
      } finally {
        if (probe === controller) {
          probe = undefined;
          starting = false;
        }
      }
    }

    const recheck = () => void check();
    const remoteLogout = () => {
      // The form remains signed out until a manual login confirms its Cookie.
      // Cancel that attempt even if check() has no connected session to recheck.
      signingIn.current?.abort();
      void check(true);
    };
    const onLogout = (event: MessageEvent) => {
      if (event.data === "logout") remoteLogout();
    };
    const onStorage = (event: StorageEvent) => {
      if (event.key === logoutSignal && event.newValue) remoteLogout();
    };
    try {
      channel.current = new BroadcastChannel(logoutSignal);
      channel.current.addEventListener("message", onLogout);
    } catch {
      // Browsers without BroadcastChannel use storage events and focus checks.
    }
    window.addEventListener("storage", onStorage);
    window.addEventListener("focus", recheck);
    window.addEventListener("pageshow", recheck);
    recheck();

    return () => {
      probe?.abort();
      signingIn.current?.abort();
      stop.current?.();
      stop.current = undefined;
      channel.current?.close();
      channel.current = undefined;
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("focus", recheck);
      window.removeEventListener("pageshow", recheck);
    };
  }, [signedIn, signedOut]);

  async function connect(token: string, remember: boolean) {
    if (signingIn.current) throw new Error("正在连接 Tend。");
    const attempt = new AbortController();
    signingIn.current = attempt;
    try {
      const notice = await loginSession(token, remember, attempt.signal);
      attempt.signal.throwIfAborted();
      signedIn(notice);
    } finally {
      signingIn.current = undefined;
    }
  }

  async function disconnect() {
    if (logoutInFlight.current) return;
    logoutInFlight.current = true;
    setEnding(true);
    const before = revision.current;
    try {
      await logoutSession();
      if (revision.current === before) signedOut("");
      if (channel.current) {
        channel.current.postMessage("logout");
      } else {
        try {
          // Only a transient event marker, not an account or a credential.
          localStorage.setItem(logoutSignal, String(Date.now()));
          localStorage.removeItem(logoutSignal);
        } catch {
          // Focus and each tab's next API request still validate the session.
        }
      }
    } catch {
      if (revision.current === before)
        setNotice("无法确认已断开连接，请恢复网络后重试。");
    } finally {
      logoutInFlight.current = false;
      setEnding(false);
    }
  }

  return { state, notice, ending, connect, disconnect };
}
