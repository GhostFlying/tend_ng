import { useState } from "react";
import type { FormEvent } from "react";
import { createRoot } from "react-dom/client";
import { readMessages } from "./api";
import { IssueBoard } from "./IssueBoard";
import { Chat } from "./Chat";
import { Notifications } from "./Notifications";
import "./style.css";

const board = window.location.pathname === "/issues";
const tokenKey = "tend.token.v1";
const chatQuery = new URLSearchParams(window.location.search).get("chat") ?? "";
const chatId = /^[a-zA-Z0-9_-]{1,128}$/.test(chatQuery) ? chatQuery : "main";
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
              aria-current={issue || board ? undefined : "page"}
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
            <a
              href="/issues"
              aria-current={board ? "page" : undefined}
              aria-label="看板"
            >
              <svg
                aria-hidden="true"
                viewBox="0 0 20 20"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinejoin="round"
              >
                <rect x="3.5" y="4.5" width="13" height="11" rx="1.5" />
                <path d="M8 5v10M12.25 5v10" />
              </svg>
              <span>看板</span>
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
            <h1>{board && token ? "Issue 看板" : "Tend"}</h1>
          )}
          {token && (
            <div className="header-actions">
              <a
                className="text-button mobile-board-link"
                href={board ? "/" : "/issues"}
              >
                {board ? "对话" : "看板"}
              </a>
              <Notifications token={token} />
              <button
                type="button"
                className="text-button"
                onClick={disconnect}
              >
                断开连接
              </button>
            </div>
          )}
        </header>
        {token ? (
          board ? (
            <IssueBoard token={token} onReachable={setReachable} />
          ) : (
            <Chat
              token={token}
              issue={issue}
              chatId={chatId}
              onReachable={setReachable}
              onTitle={setTitle}
            />
          )
        ) : (
          <Connect onConnect={connect} />
        )}
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
