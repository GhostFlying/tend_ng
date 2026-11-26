import { useSyncExternalStore } from "react";

export const issuePath =
  /^\/issues\/(%23[1-9][0-9]*|[a-zA-Z0-9_-]+)(?:\/overview)?$/;

const pages = new Set([
  "/",
  "/discussions",
  "/issues",
  "/agents",
  "/hosts",
  "/more",
]);
const changed = "tend:navigate";

function subscribe(listener: () => void) {
  window.addEventListener(changed, listener);
  window.addEventListener("popstate", listener);
  window.addEventListener("hashchange", listener);
  return () => {
    window.removeEventListener(changed, listener);
    window.removeEventListener("popstate", listener);
    window.removeEventListener("hashchange", listener);
  };
}

function currentURL() {
  return window.location.href;
}

export function usePageURL() {
  return new URL(useSyncExternalStore(subscribe, currentURL));
}

function samePage(a: URL, b: URL) {
  return a.pathname === b.pathname && a.search === b.search;
}

export function navigate(href: string) {
  const current = new URL(currentURL());
  const destination = new URL(href, current);
  if (destination.href === current.href) return;
  window.history.pushState(null, "", destination);
  window.dispatchEvent(new Event(changed));
  if (current.hash !== destination.hash) {
    window.dispatchEvent(
      new HashChangeEvent("hashchange", {
        oldURL: current.href,
        newURL: destination.href,
      }),
    );
  }
}

export function pageLink(anchor: HTMLAnchorElement): string | null {
  if (
    !anchor.hasAttribute("href") ||
    anchor.hasAttribute("download") ||
    (anchor.target && anchor.target.toLowerCase() !== "_self")
  )
    return null;
  const current = new URL(currentURL());
  let destination: URL;
  try {
    destination = new URL(anchor.href);
  } catch {
    return null;
  }
  if (
    destination.origin !== current.origin ||
    (!pages.has(destination.pathname) && !issuePath.test(destination.pathname))
  )
    return null;
  // Keep normal fragment scrolling and hashchange handling within the page.
  return samePage(current, destination) && destination.hash
    ? null
    : destination.href;
}
