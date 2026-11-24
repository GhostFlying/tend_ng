import { useState } from "react";
import type { FormEvent } from "react";
import { createRoot } from "react-dom/client";
import { readMessages } from "./api";
import { Chat } from "./Chat";
import "./style.css";

const tokenKey = "tend.token.v1";
const issue = /^\/issues\/([a-zA-Z0-9_-]+)$/.exec(
  window.location.pathname,
)?.[1];

function Connect({ onConnect }: { onConnect: (token: string) => void }) {
  const [token, setToken] = useState("");
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState("");

  async function connect(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = token.trim();
    if (!value || checking) return;
    setError("");
    setChecking(true);
    try {
      await readMessages(value);
      onConnect(value);
    } catch (error) {
      setError(error instanceof Error ? error.message : "暂时无法连接 Tend");
    } finally {
      setChecking(false);
    }
  }

  return (
    <main className="lane connect">
      <h1>连接 Tend</h1>
      <form onSubmit={connect} className="connect-form">
        <label htmlFor="token">访问令牌</label>
        <input
          id="token"
          type="password"
          autoComplete="current-password"
          value={token}
          required
          onChange={(event) => setToken(event.target.value)}
        />
        <button type="submit" className="primary" disabled={checking}>
          {checking ? "正在连接…" : "连接"}
        </button>
      </form>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </main>
  );
}

function App() {
  const [token, setToken] = useState(() => sessionStorage.getItem(tokenKey));
  const [reachable, setReachable] = useState<boolean | null>(null);
  const [title, setTitle] = useState("");

  function connect(value: string) {
    sessionStorage.setItem(tokenKey, value);
    setReachable(true);
    setToken(value);
  }

  function disconnect() {
    sessionStorage.removeItem(tokenKey);
    setReachable(null);
    setToken(null);
  }

  return (
    <div className={token ? "app connected" : "app"}>
      {token && (
        <aside className="sidebar">
          <div className="brand">
            <span className="brand-mark" aria-hidden="true">
              t
            </span>
            <strong>tend</strong>
            <span
              role="status"
              className="connection"
              data-online={reachable === true}
            >
              {reachable === null
                ? "连接中"
                : reachable
                  ? "已连接"
                  : "连接中断"}
            </span>
          </div>
          <nav aria-label="主导航">
            <a
              href="/"
              aria-current={issue ? undefined : "page"}
              aria-label="对话"
            >
              <svg
                aria-hidden="true"
                viewBox="0 0 20 20"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinejoin="round"
              >
                <path d="M3.5 4.5h13v9h-8l-4.5 3v-3H3.5z" />
              </svg>
              <span>对话</span>
            </a>
          </nav>
        </aside>
      )}
      <div className="content">
        <header className="header">
          {token && issue ? (
            <div className="issue-heading">
              <a href="/" className="text-button">
                对话
              </a>
              <span aria-hidden="true">/</span>
              <h1>{title || "Issue 对话"}</h1>
            </div>
          ) : (
            <h1>Tend</h1>
          )}
          {token && (
            <button type="button" className="text-button" onClick={disconnect}>
              断开连接
            </button>
          )}
        </header>
        {token ? (
          <Chat
            token={token}
            issue={issue}
            onReachable={setReachable}
            onTitle={setTitle}
          />
        ) : (
          <Connect onConnect={connect} />
        )}
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
