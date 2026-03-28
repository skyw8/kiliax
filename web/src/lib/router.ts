import { useMemo, useSyncExternalStore } from "react";

const NAV_EVENT = "kiliax:navigate";

function subscribe(callback: () => void) {
  window.addEventListener("popstate", callback);
  window.addEventListener(NAV_EVENT, callback);
  return () => {
    window.removeEventListener("popstate", callback);
    window.removeEventListener(NAV_EVENT, callback);
  };
}

function getPathname() {
  return window.location.pathname;
}

export type Route =
  | { name: "home" }
  | { name: "session"; sessionId: string };

export function parseRoute(pathname: string): Route {
  const cleaned = pathname.replace(/\/+$/, "") || "/";
  if (cleaned === "/" || cleaned === "/sessions") return { name: "home" };

  const m = cleaned.match(/^\/sessions\/([^/]+)$/);
  if (m) {
    return { name: "session", sessionId: decodeURIComponent(m[1]) };
  }

  return { name: "home" };
}

export function useRoute(): Route {
  const pathname = useSyncExternalStore(subscribe, getPathname, getPathname);
  return useMemo(() => parseRoute(pathname), [pathname]);
}

export function navigate(path: string, opts?: { replace?: boolean }) {
  if (opts?.replace) {
    window.history.replaceState({}, "", path);
  } else {
    window.history.pushState({}, "", path);
  }
  window.dispatchEvent(new Event(NAV_EVENT));
}

export function hrefToSession(sessionId: string) {
  return `/sessions/${encodeURIComponent(sessionId)}`;
}

