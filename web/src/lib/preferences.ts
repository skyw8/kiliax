import { isOverlaySidebarViewport } from "./app-utils";

const PINNED_SESSIONS_KEY = "kiliax:pinned_session_ids";
const PINNED_WORKSPACES_KEY = "kiliax:pinned_workspace_roots";
const SIDEBAR_OPEN_KEY = "kiliax:sidebar_open";
const WORKSPACES_OPEN_KEY = "kiliax:sidebar_workspaces_open";
const SESSIONS_OPEN_KEY = "kiliax:sidebar_sessions_open";

export function loadPinnedSessionIds(): string[] {
  try {
    const raw = localStorage.getItem(PINNED_SESSIONS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((v) => typeof v === "string");
  } catch {
    return [];
  }
}

export function savePinnedSessionIds(ids: string[]) {
  try {
    localStorage.setItem(PINNED_SESSIONS_KEY, JSON.stringify(ids));
  } catch {
    // ignore
  }
}

export function loadPinnedWorkspaceRoots(): string[] {
  try {
    const raw = localStorage.getItem(PINNED_WORKSPACES_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((v) => typeof v === "string");
  } catch {
    return [];
  }
}

export function savePinnedWorkspaceRoots(roots: string[]) {
  try {
    localStorage.setItem(PINNED_WORKSPACES_KEY, JSON.stringify(roots));
  } catch {
    // ignore
  }
}

export function loadSidebarOpen(): boolean {
  if (isOverlaySidebarViewport()) return false;
  try {
    const raw = localStorage.getItem(SIDEBAR_OPEN_KEY);
    if (!raw) return true;
    return raw !== "0" && raw !== "false";
  } catch {
    return true;
  }
}

export function saveSidebarOpen(open: boolean) {
  try {
    localStorage.setItem(SIDEBAR_OPEN_KEY, open ? "1" : "0");
  } catch {
    // ignore
  }
}

export function loadWorkspacesPaneOpen(defaultValue = true): boolean {
  return loadSidebarSectionOpen(WORKSPACES_OPEN_KEY, defaultValue);
}

export function saveWorkspacesPaneOpen(open: boolean) {
  saveSidebarSectionOpen(WORKSPACES_OPEN_KEY, open);
}

export function loadSessionsPaneOpen(defaultValue = true): boolean {
  return loadSidebarSectionOpen(SESSIONS_OPEN_KEY, defaultValue);
}

export function saveSessionsPaneOpen(open: boolean) {
  saveSidebarSectionOpen(SESSIONS_OPEN_KEY, open);
}

function loadSidebarSectionOpen(key: string, defaultValue: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return defaultValue;
    return raw !== "0" && raw !== "false";
  } catch {
    return defaultValue;
  }
}

function saveSidebarSectionOpen(key: string, open: boolean) {
  try {
    localStorage.setItem(key, open ? "1" : "0");
  } catch {
    // ignore
  }
}

