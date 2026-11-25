import { lazy, Suspense, useState } from "react";
import type { FormEvent } from "react";
import { createRoot } from "react-dom/client";
import { readMessages } from "./api";
import { IssueBoard } from "./IssueBoard";
import { Chat } from "./Chat";
import { Notifications } from "./Notifications";
import { Agents, Hosts } from "./Resources";
import {
  MoreResources,
  NavIcon,
  PhoneNavigation,
  resourceLinks,
} from "./ResourceNavigation";
import "./style.css";
import "./issue-detail.css";

const IssueDetail = lazy(() =>
  import("./IssueDetail").then(({ IssueDetail }) => ({ default: IssueDetail })),
);

const path = window.location.pathname;
const board = path === "/issues";
const resourceTitle =
  path === "/agents"
    ? "Agents"
    : path === "/hosts"
      ? "执行主机"
      : path === "/more"
        ? "更多"
        : "";
const tokenKey = "tend.token.v1";
const chatQuery = new URLSearchParams(window.location.search).get("chat") ?? "";
const chatId = /^[a-zA-Z0-9_-]{1,128}$/.test(chatQuery) ? chatQuery : "main";
const issueRoute =
  /^\/issues\/(%23[1-9][0-9]*|[a-zA-Z0-9_-]+)(?:\/overview)?$/.exec(path);
const issue = issueRoute ? decodeURIComponent(issueRoute[1]) : undefined;
const issueNumber = issue?.match(/^#?([1-9][0-9]*)$/)?.[1];

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
              aria-current={
                issue || board || resourceTitle ? undefined : "page"
              }
              aria-label="对话"
            >
              <NavIcon name="chat" />
              <span>对话</span>
            </a>
            <a
              href="/issues"
              aria-current={board || issue ? "page" : undefined}
              aria-label="看板"
            >
              <NavIcon name="board" />
              <span>看板</span>
            </a>
            <div className="resource-nav">
              <h2>资源</h2>
              {resourceLinks.map(({ href, label, icon }) => (
                <a
                  key={href}
                  href={href}
                  aria-label={label}
                  aria-current={path === href ? "page" : undefined}
                >
                  <NavIcon name={icon} />
                  <span>{label}</span>
                </a>
              ))}
            </div>
          </nav>
        </aside>
      )}
      <div className="content" data-issue={!!issue}>
        <header
          className={
            token && resourceTitle ? "header resource-header" : "header"
          }
          data-resource={token && resourceTitle ? path.slice(1) : undefined}
        >
          {token && issue ? (
            <nav className="issue-crumb" aria-label="当前位置">
              <a href="/issues" className="text-button">
                返回看板
              </a>
              <span aria-hidden="true">/</span>
              <span>
                {issueNumber ? `Issue #${issueNumber}` : "Issue 详情"}
              </span>
            </nav>
          ) : (
            <h1>
              {(token && (resourceTitle || (board && "Issue 看板"))) || "Tend"}
            </h1>
          )}
          {token && !resourceTitle && (
            <div className="header-actions">
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
          ) : issue ? (
            <Suspense
              fallback={
                <main className="lane chat" role="status">
                  正在读取 Issue 详情…
                </main>
              }
            >
              <IssueDetail
                token={token}
                issue={issue}
                chatId={chatId}
                onReachable={setReachable}
              />
            </Suspense>
          ) : path === "/agents" ? (
            <Agents token={token} onReachable={setReachable} />
          ) : path === "/hosts" ? (
            <Hosts token={token} onReachable={setReachable} />
          ) : path === "/more" ? (
            <MoreResources disconnect={disconnect} />
          ) : (
            <Chat token={token} chatId={chatId} onReachable={setReachable} />
          )
        ) : (
          <Connect onConnect={connect} />
        )}
        {token && (
          <PhoneNavigation
            token={token}
            resource={!!resourceTitle}
            board={board || !!issue}
          />
        )}
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
