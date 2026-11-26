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

function subscribe(listener: (event: Event) => void) {
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

// Mutations may finish after a user leaves and even after they return to the
// same URL. Only the initiating visit may choose the next page.
export function trackPageVisit() {
  const href = currentURL();
  let stayed = true;
  const unsubscribe = subscribe((event) => {
    // Traversal can activate a different history entry with the same URL.
    if (event.type === "popstate" || currentURL() !== href) stayed = false;
  });
  return () => {
    unsubscribe();
    return stayed && currentURL() === href;
  };
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

export function replacePageQuery(values: Record<string, string>) {
  const url = new URL(currentURL());
  for (const [key, value] of Object.entries(values)) {
    if (value) url.searchParams.set(key, value);
    else url.searchParams.delete(key);
  }
  if (url.href === currentURL()) return;
  window.history.replaceState(window.history.state, "", url);
  window.dispatchEvent(new Event(changed));
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
