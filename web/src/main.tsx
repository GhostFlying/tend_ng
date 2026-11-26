import { lazy, Suspense, useState } from "react";
import type { FormEvent } from "react";
import { createRoot } from "react-dom/client";
import { allowsWebLogin } from "./webSession";
import { useWebSession } from "./useWebSession";
import { IssueBoard } from "./IssueBoard";
import { Chat } from "./Chat";
import { DiscussionStart } from "./DiscussionStart";
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

const DiscussionSidebar = lazy(() =>
  import("./Discussions").then(({ DiscussionSidebar }) => ({
    default: DiscussionSidebar,
  })),
);
const DiscussionPage = lazy(() =>
  import("./Discussions").then(({ DiscussionPage }) => ({
    default: DiscussionPage,
  })),
);

const path = window.location.pathname;
const board = path === "/issues";
const discussions = path === "/discussions";
const resourceTitle =
  path === "/agents"
    ? "Agents"
    : path === "/hosts"
      ? "执行主机"
      : path === "/more"
        ? "更多"
        : "";
const query = new URLSearchParams(window.location.search);
const chatQuery = query.get("chat") ?? "";
const archivedDiscussions = query.get("archived") === "true";
const chatId = /^[a-zA-Z0-9_-]{1,128}$/.test(chatQuery) ? chatQuery : "main";
const discussionChat = chatQuery && chatId !== "main" ? chatId : null;
const issueRoute =
  /^\/issues\/(%23[1-9][0-9]*|[a-zA-Z0-9_-]+)(?:\/overview)?$/.exec(path);
const issue = issueRoute ? decodeURIComponent(issueRoute[1]) : undefined;
const issueNumber = issue?.match(/^#?([1-9][0-9]*)$/)?.[1];

function Connect({
  notice,
  onConnect,
}: {
  notice: string;
  onConnect: (token: string, remember: boolean) => Promise<void>;
}) {
  const [token, setToken] = useState("");
  const canRemember = window.location.protocol === "https:";
  const [remember, setRemember] = useState(false);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState("");

  async function connect(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const value = token.trim();
    if (!value || checking) return;
    setError("");
    setChecking(true);
    try {
      await onConnect(value, canRemember && remember);
      setToken("");
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") {
        setToken("");
        setError("另一标签已退出，请重新连接 Tend。");
      } else {
        setError(error instanceof Error ? error.message : "暂时无法连接 Tend");
      }
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
          autoComplete="off"
          value={token}
          required
          onChange={(event) => setToken(event.target.value)}
        />
        <label className="remember-browser">
          <input
            type="checkbox"
            checked={remember}
            disabled={!canRemember || checking}
            aria-describedby="remember-hint"
            onChange={(event) => setRemember(event.target.checked)}
          />
          记住此浏览器（7 天）
        </label>
        <p id="remember-hint" className="session-hint">
          {canRemember
            ? "不勾选时，仅在本次浏览器会话内有效，最长 12 小时。"
            : "关闭并重新打开浏览器后保留登录，需要 HTTPS。本机 HTTP 会话最长 12 小时。"}
        </p>
        <button type="submit" className="primary" disabled={checking}>
          {checking ? "正在连接…" : "连接"}
        </button>
      </form>
      {(error || notice) && (
        <p className="error" role="alert">
          {error || notice}
        </p>
      )}
    </main>
  );
}

function App() {
  const { state, notice, ending, connect, disconnect } = useWebSession();
  const [reachable, setReachable] = useState<boolean | null>(null);
  const connected = state === "connected";

  async function handleConnect(token: string, remember: boolean) {
    await connect(token, remember);
    setReachable(true);
  }

  return (
    <div className={connected ? "app connected" : "app"}>
      {connected && (
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
                issue || board || discussions || resourceTitle
                  ? undefined
                  : "page"
              }
              aria-label="对话"
            >
              <NavIcon name="chat" />
              <span>对话</span>
            </a>
            <div className="discussion-nav-row">
              <a
                href="/discussions"
                aria-current={
                  discussions && !discussionChat ? "page" : undefined
                }
                aria-label="讨论"
              >
                <NavIcon name="discussion" />
                <span>讨论</span>
              </a>
              <DiscussionStart sidebar />
            </div>
            {discussions && (
              <Suspense fallback={null}>
                <DiscussionSidebar chat={discussionChat} />
              </Suspense>
            )}
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
            connected && resourceTitle ? "header resource-header" : "header"
          }
          data-resource={connected && resourceTitle ? path.slice(1) : undefined}
        >
          {connected && issue ? (
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
              {(connected &&
                (resourceTitle ||
                  (board && "Issue 看板") ||
                  (discussions && "讨论"))) ||
                "Tend"}
            </h1>
          )}
          {connected && !resourceTitle && (
            <div className="header-actions">
              <Notifications />
              <button
                type="button"
                className="text-button"
                disabled={ending}
                onClick={() => void disconnect()}
              >
                断开连接
              </button>
            </div>
          )}
        </header>
        {connected && notice && (
          <p className="session-notice" role="status">
            {notice}
          </p>
        )}
        {connected ? (
          board ? (
            <IssueBoard onReachable={setReachable} />
          ) : issue ? (
            <Suspense
              fallback={
                <main className="lane chat" role="status">
                  正在读取 Issue 详情…
                </main>
              }
            >
              <IssueDetail
                issue={issue}
                chatId={chatId}
                onReachable={setReachable}
              />
            </Suspense>
          ) : discussions ? (
            <Suspense
              fallback={
                <main className="lane chat" role="status">
                  正在读取讨论…
                </main>
              }
            >
              <DiscussionPage
                chat={discussionChat}
                archived={archivedDiscussions}
                onReachable={setReachable}
              />
            </Suspense>
          ) : path === "/agents" ? (
            <Agents onReachable={setReachable} />
          ) : path === "/hosts" ? (
            <Hosts onReachable={setReachable} />
          ) : path === "/more" ? (
            <MoreResources
              disconnect={() => void disconnect()}
              ending={ending}
            />
          ) : (
            <Chat chatId={chatId} onReachable={setReachable} />
          )
        ) : state === "checking" ? (
          <main className="lane connect" role="status">
            正在检查登录状态…
          </main>
        ) : (
          <Connect notice={notice} onConnect={handleConnect} />
        )}
        {connected && (
          <PhoneNavigation
            resource={!!resourceTitle}
            board={board || !!issue}
            discussions={discussions}
          />
        )}
      </div>
    </div>
  );
}

function HttpsRequired() {
  return (
    <div className="app">
      <div className="content">
        <header className="header">
          <h1>Tend</h1>
        </header>
        <main className="lane connect">
          <h1>请使用 HTTPS 登录 Tend</h1>
          <p className="session-hint">
            当前是远端 HTTP 地址。请打开已配置的 HTTPS 地址后再输入访问令牌。
          </p>
        </main>
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(
  allowsWebLogin(location) ? <App /> : <HttpsRequired />,
);
