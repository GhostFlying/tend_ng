import { Notifications } from "./Notifications";

type IconName =
  | "chat"
  | "discussion"
  | "board"
  | "agents"
  | "hosts"
  | "usage"
  | "more"
  | "notice";

export function NavIcon({ name }: { name: IconName }) {
  const shape = {
    chat: <path d="M3.5 4.5h13v9h-8l-4.5 3v-3H3.5z" />,
    discussion: <path d="M3.5 4.5h13v9h-8l-4.5 3v-3H3.5zM7 8h6m-6 2.5h4" />,
    board: (
      <>
        <rect x="3.5" y="4.5" width="13" height="11" rx="1.5" />
        <path d="M8 5v10M12.25 5v10" />
      </>
    ),
    agents: (
      <>
        <circle cx="8" cy="6" r="2.5" />
        <path d="M2.8 17v-2a5.2 5.2 0 0 1 10.4 0v2M13 3.7a2.5 2.5 0 0 1 0 4.6m2 2a4.4 4.4 0 0 1 2.2 3.8V17" />
      </>
    ),
    hosts: (
      <>
        <rect x="3" y="3.8" width="14" height="5" rx="1.2" />
        <rect x="3" y="11.2" width="14" height="5" rx="1.2" />
      </>
    ),
    usage: <path d="M3.5 16.5h13m-11-3v-3m4.5 3V7m4.5 6.5v-10" />,
    more: (
      <>
        <circle cx="4.5" cy="10" r="1" />
        <circle cx="10" cy="10" r="1" />
        <circle cx="15.5" cy="10" r="1" />
      </>
    ),
    notice: (
      <>
        <circle cx="10" cy="10" r="7" />
        <path d="M10 6.5v4m0 2.7v.3" />
      </>
    ),
  };
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 20 20"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinejoin="round"
      strokeLinecap="round"
    >
      {shape[name]}
    </svg>
  );
}

export const resourceLinks = [
  { href: "/agents", label: "Agents", icon: "agents" },
  { href: "/hosts", label: "主机", icon: "hosts" },
  { href: "/token-usage", label: "Token 用量", icon: "usage" },
] as const;

export function PhoneNavigation({
  resource,
  board,
  discussions,
}: {
  resource: boolean;
  board: boolean;
  discussions: boolean;
}) {
  return (
    <nav className="phone-nav" aria-label="手机导航">
      <a
        className="phone-item"
        href="/"
        aria-current={!resource && !board && !discussions ? "page" : undefined}
      >
        <NavIcon name="chat" />
        对话
      </a>
      <a
        className="phone-item"
        href="/issues"
        aria-label="看板"
        aria-current={board ? "page" : undefined}
      >
        <NavIcon name="board" />
        Issues
      </a>
      <a
        className="phone-item"
        href="/discussions"
        aria-current={discussions ? "page" : undefined}
      >
        <NavIcon name="discussion" />
        讨论
      </a>
      <Notifications mobileIcon={<NavIcon name="notice" />} />
      <a
        className="phone-item"
        href="/more"
        aria-current={resource ? "page" : undefined}
      >
        <NavIcon name="more" />
        更多
      </a>
    </nav>
  );
}

export function MoreResources({
  disconnect,
  ending,
}: {
  disconnect: () => void;
  ending: boolean;
}) {
  return (
    <main className="resource-page">
      <nav className="more-resources" aria-label="更多导航">
        {resourceLinks.map(({ href, label, icon }) => (
          <a key={href} href={href}>
            <NavIcon name={icon} />
            {label}
          </a>
        ))}
        <button type="button" onClick={disconnect} disabled={ending}>
          断开连接
        </button>
      </nav>
    </main>
  );
}
