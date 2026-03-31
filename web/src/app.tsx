import React, { useEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, ArrowDown, ArrowLeft, ArrowUp, ChevronDown, ChevronRight, Code, Copy, FolderOpen, FolderPlus, GitFork, MoreHorizontal, PanelLeftClose, PanelLeftOpen, Pin, Plus, Plug, RefreshCcw, Settings, Sparkles, Square, Terminal, Trash2, X } from "lucide-react";
import { api, ApiError, wsUrl } from "@/lib/api";
import { hrefToSession, navigate, useRoute } from "@/lib/router";
import type {
  Capabilities,
  FsEntry,
  Message,
  Session,
  SessionSummary,
  SkillSummary,
  ToolCall,
} from "@/lib/types";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { CodeBlock } from "@/components/code-block";
import { Markdown, type MermaidErrorInfo } from "@/components/markdown";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { Textarea } from "@/components/ui/textarea";

type PendingMessage = {
  sessionId: string;
  runId: string;
  baseEventId: number;
  createdAt: string;
  content: string;
};

type StreamState = {
  thinking: string;
  assistant: string;
  assistantStarted: boolean;
  toolCalls: Array<{ id: string; name: string; arguments: string }>;
};

type AlertItem = {
  id: string;
  title: string;
  subtitle?: string;
  message: string;
  traceId?: string;
  details?: unknown;
  autoCloseMs?: number;
};

const PINNED_SESSIONS_KEY = "kiliax:pinned_session_ids";
const PINNED_WORKSPACES_KEY = "kiliax:pinned_workspace_roots";
const SIDEBAR_OPEN_KEY = "kiliax:sidebar_open";
const WORKSPACES_OPEN_KEY = "kiliax:sidebar_workspaces_open";
const SESSIONS_OPEN_KEY = "kiliax:sidebar_sessions_open";
const LIST_PAGE_SIZE = 6;

function displayModelId(modelId: string): string {
  const idx = modelId.indexOf("/");
  return idx === -1 ? modelId : modelId.slice(idx + 1);
}

function hasMermaidFence(text?: string | null): boolean {
  return /(^|\n)```[ \t]*mermaid\b/i.test(text ?? "");
}

function stringifyUnknown(v: unknown): string {
  if (v == null) return "";
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v, null, 2);
  } catch {
    return String(v);
  }
}

function newAlertId(prefix: string): string {
  return `${prefix}_${Date.now().toString(16)}_${Math.random().toString(16).slice(2)}`;
}

function monotonicNowMs(): number {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return performance.now();
  }
  return Date.now();
}

function fmtDurationCompact(durationMs: number): string {
  const ms = Math.max(0, Math.round(durationMs));
  if (ms >= 60_000) {
    const sec = Math.floor(ms / 1000);
    const minutes = Math.floor(sec / 60);
    const seconds = sec % 60;
    return `${minutes}m${String(seconds).padStart(2, "0")}s`;
  }
  if (ms >= 1_000) return `${(ms / 1_000).toFixed(1)}s`;
  return `${ms}ms`;
}

async function copyToClipboard(text: string): Promise<boolean> {
  const value = text ?? "";
  try {
    await navigator.clipboard.writeText(value);
    return true;
  } catch {
    try {
      const el = document.createElement("textarea");
      el.value = value;
      el.style.position = "fixed";
      el.style.left = "-9999px";
      el.style.top = "0";
      document.body.appendChild(el);
      el.focus();
      el.select();
      const ok = document.execCommand("copy");
      document.body.removeChild(el);
      return ok;
    } catch {
      return false;
    }
  }
}

function AlertStack({
  items,
  onClose,
}: {
  items: AlertItem[];
  onClose: (id: string) => void;
}) {
  if (!items.length) return null;

  return (
    <div className="fixed bottom-6 right-6 z-50 flex w-[min(560px,calc(100vw-24px))] flex-col gap-3">
      {items.map((a) => (
        <Alert key={a.id} variant="destructive" className="shadow-lg">
          <div className="flex items-start justify-between gap-3">
            <div className="flex min-w-0 items-start gap-2">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-red-600" />
              <div className="min-w-0">
                <div className="truncate text-sm font-semibold text-zinc-900">
                  {a.title}
                </div>
                {a.subtitle ? (
                  <div className="mt-0.5 truncate text-xs text-zinc-600">
                    {a.subtitle}
                  </div>
                ) : null}
              </div>
            </div>
            <button
              type="button"
              className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
              aria-label="Close"
              onClick={() => onClose(a.id)}
            >
              <X className="h-4 w-4" />
            </button>
          </div>

          <pre className="mt-3 max-h-[32vh] overflow-auto whitespace-pre-wrap rounded-md bg-zinc-50 p-3 text-xs text-zinc-800">
            {a.message}
          </pre>

          {a.traceId ? (
            <div className="mt-2 text-xs text-zinc-600">
              trace_id: <span className="font-mono">{a.traceId}</span>
            </div>
          ) : null}

          {a.details != null ? (
            <details className="mt-2 rounded-md border border-zinc-200 bg-white px-3 py-2">
              <summary className="cursor-pointer select-none text-xs text-zinc-700">
                details
              </summary>
              <pre className="mt-2 max-h-[32vh] overflow-auto rounded bg-zinc-50 p-2 text-xs text-zinc-800">
                {stringifyUnknown(a.details)}
              </pre>
            </details>
          ) : null}
        </Alert>
      ))}
    </div>
  );
}

function loadPinnedSessionIds(): string[] {
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

function savePinnedSessionIds(ids: string[]) {
  try {
    localStorage.setItem(PINNED_SESSIONS_KEY, JSON.stringify(ids));
  } catch {
    // ignore
  }
}

function loadPinnedWorkspaceRoots(): string[] {
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

function savePinnedWorkspaceRoots(roots: string[]) {
  try {
    localStorage.setItem(PINNED_WORKSPACES_KEY, JSON.stringify(roots));
  } catch {
    // ignore
  }
}

function loadSidebarOpen(): boolean {
  try {
    const raw = localStorage.getItem(SIDEBAR_OPEN_KEY);
    if (!raw) return true;
    return raw !== "0" && raw !== "false";
  } catch {
    return true;
  }
}

function saveSidebarOpen(open: boolean) {
  try {
    localStorage.setItem(SIDEBAR_OPEN_KEY, open ? "1" : "0");
  } catch {
    // ignore
  }
}

function loadSidebarSectionOpen(key: string, defaultValue = true): boolean {
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

function statusBadge(summary: SessionSummary): {
  label: string;
  variant: "idle" | "step" | "done" | "error";
} {
  const runState = summary.status.run_state;
  if (runState === "running" || runState === "tooling") {
    const step = summary.status.step > 0 ? summary.status.step : 1;
    return { label: `step ${step}`, variant: "step" };
  }
  if (summary.last_outcome === "error") return { label: "error", variant: "error" };
  if (summary.last_outcome === "done") return { label: "done", variant: "done" };
  return { label: "idle", variant: "idle" };
}

function sortSessions(items: SessionSummary[], pinnedIds: string[]): SessionSummary[] {
  const pinnedRank = new Map(pinnedIds.map((id, idx) => [id, idx]));
  return [...items].sort((a, b) => {
    const aPinned = pinnedRank.has(a.id);
    const bPinned = pinnedRank.has(b.id);
    if (aPinned !== bPinned) return aPinned ? -1 : 1;
    if (aPinned && bPinned) {
      return (pinnedRank.get(a.id) ?? 0) - (pinnedRank.get(b.id) ?? 0);
    }

    const aRunning =
      a.status.run_state === "running" || a.status.run_state === "tooling";
    const bRunning =
      b.status.run_state === "running" || b.status.run_state === "tooling";
    if (aRunning !== bRunning) return aRunning ? -1 : 1;
    return b.updated_at.localeCompare(a.updated_at);
  });
}

function normalizePathForMatch(p: string): string {
  return (p ?? "").replaceAll("\\", "/");
}

function workspaceBasename(root: string): string {
  const normalized = normalizePathForMatch(root).replace(/\/+$/, "");
  if (!normalized) return root;
  if (normalized === "/") return "/";
  const idx = normalized.lastIndexOf("/");
  const base = idx === -1 ? normalized : normalized.slice(idx + 1);
  return base || normalized;
}

function isTmpWorkspaceRoot(root: string): boolean {
  const p = normalizePathForMatch(root).toLowerCase();
  return p.includes("/.kiliax/workspace/tmp_");
}

function renderToolCalls(
  toolCalls: ToolCall[] | undefined,
  toolDurationsMs: Record<string, number>,
) {
  if (!toolCalls?.length) return null;
  return (
    <div className="mt-2 space-y-1">
      {toolCalls.map((c) => (
        <details
          key={c.id}
          className="relative rounded-md border border-zinc-200 bg-white px-3 py-2"
        >
          <button
            type="button"
            className="absolute right-1 top-1 rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
            aria-label="Copy tool call"
            title="Copy tool call"
            onClick={() => copyToClipboard(c.arguments)}
          >
            <Copy className="h-3.5 w-3.5" />
          </button>
          <summary className="cursor-pointer select-none pr-10 text-xs text-zinc-700">
            tool_call: <span className="font-mono">{c.name}</span>
            {toolDurationsMs[c.id] != null ? (
              <span className="ml-2 text-zinc-500">({fmtDurationCompact(toolDurationsMs[c.id]!)})</span>
            ) : null}
          </summary>
          <CodeBlock className="mt-2" code={c.arguments} lang="json" copyButton={false} />
        </details>
      ))}
    </div>
  );
}

function FolderPicker({
  path,
  onPathChange,
}: {
  path: string;
  onPathChange: (next: string) => void;
}) {
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [parent, setParent] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reloadSeq, setReloadSeq] = useState(0);
  const skipNextFetchPathRef = useRef<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const handle = window.setTimeout(() => {
      if (cancelled) return;
      async function load() {
        if (skipNextFetchPathRef.current === path) {
          skipNextFetchPathRef.current = null;
          return;
        }

        setLoading(true);
        setError(null);
        try {
          const res = await api.fsList(path.trim() ? path.trim() : undefined);
          if (cancelled) return;
          setEntries(res.entries ?? []);
          setParent(res.parent ?? null);
          setLoading(false);
          if (res.path && res.path !== path) {
            skipNextFetchPathRef.current = res.path;
            onPathChange(res.path);
          }
        } catch (err) {
          if (cancelled) return;
          setLoading(false);
          const msg =
            err instanceof ApiError
              ? err.message
              : err instanceof Error
                ? err.message
                : "Failed to list folders";
          setError(msg);
        }
      }
      void load();
    }, 150);
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [path, reloadSeq, onPathChange]);

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2 rounded-md border border-zinc-200 bg-white px-2 py-1">
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8"
          aria-label="Up"
          title="Up"
          disabled={!parent || loading}
          onClick={() => {
            if (!parent) return;
            onPathChange(parent);
          }}
        >
          <ArrowLeft className="h-4 w-4 text-zinc-600" />
        </Button>
        <Input
          value={path}
          onChange={(e) => onPathChange(e.target.value)}
          placeholder="/path/to/folder"
          aria-label="Path"
          title={path}
          className="h-8 min-w-0 flex-1 px-2 py-1 font-mono text-xs"
        />
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8"
          aria-label="Refresh"
          title="Refresh"
          disabled={loading}
          onClick={() => setReloadSeq((v) => v + 1)}
        >
          <RefreshCcw className="h-4 w-4 text-zinc-600" />
        </Button>
      </div>

      {error ? (
        <div className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
          {error}
        </div>
      ) : null}

      <div className="h-[min(320px,50vh)] overflow-auto rounded-md border border-zinc-200 bg-white">
        {loading ? (
          <div className="flex h-full items-center justify-center px-3 text-center text-xs text-zinc-500">
            Loading…
          </div>
        ) : entries.length ? (
          <div className="divide-y divide-zinc-200">
            {entries.map((e) => (
              <button
                key={e.path}
                type="button"
                className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-zinc-50"
                onClick={() => onPathChange(e.path)}
              >
                <ChevronRight className="h-4 w-4 shrink-0 text-zinc-500" />
                <div className="min-w-0 flex-1 truncate font-mono text-xs text-zinc-800">
                  {e.name}
                </div>
              </button>
            ))}
          </div>
        ) : (
          <div className="flex h-full items-center justify-center px-3 text-center text-xs text-zinc-500">
            No folders
          </div>
        )}
      </div>
    </div>
  );
}

function FolderPickerDialog({
  open,
  onOpenChange,
  title,
  description,
  path,
  onPathChange,
  confirmLabel,
  confirmDisabled,
  confirmPending,
  confirmPendingLabel,
  onConfirm,
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  path: string;
  onPathChange: (next: string) => void;
  confirmLabel: string;
  confirmDisabled?: boolean;
  confirmPending?: boolean;
  confirmPendingLabel?: string;
  onConfirm: () => void | Promise<void>;
  children?: React.ReactNode;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description ? <DialogDescription>{description}</DialogDescription> : null}
        </DialogHeader>
        <div className="space-y-3">
          {children}
          <FolderPicker path={path} onPathChange={onPathChange} />
          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button onClick={onConfirm} disabled={confirmDisabled}>
              {confirmPending ? (confirmPendingLabel ?? confirmLabel) : confirmLabel}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function MessageRow({
  msg,
  toolDurationsMs,
  thinkingDurationsMs,
  assistantDurationsMs,
  onMermaidError,
  onFork,
}: {
  msg: Message;
  toolDurationsMs: Record<string, number>;
  thinkingDurationsMs: Record<string, number>;
  assistantDurationsMs: Record<string, number>;
  onMermaidError?: (info: MermaidErrorInfo) => void;
  onFork?: (assistantMessageId: string) => void;
}) {
  if (msg.role === "user") {
    const wide = hasMermaidFence(msg.content);
    const bubbleWidth = wide ? "w-full max-w-[92%]" : "max-w-[78%]";
    return (
      <div className="flex justify-end">
        <div className={`${bubbleWidth} rounded-2xl bg-zinc-900 px-4 py-2 text-sm text-zinc-50`}>
          {msg.content}
        </div>
      </div>
    );
  }

  if (msg.role === "assistant") {
    const wide = hasMermaidFence(msg.content);
    const bubbleWidth = wide ? "w-full max-w-[92%]" : "max-w-[78%]";
    return (
      <div className="flex justify-start">
        <div className={`${bubbleWidth} rounded-2xl bg-zinc-50 px-4 py-2 text-sm text-zinc-900`}>
          {msg.content ? (
            <Markdown text={msg.content} messageId={msg.id} onMermaidError={onMermaidError} />
          ) : (
            <div className="text-zinc-500">…</div>
          )}
          {msg.reasoning_content ? (
            <details className="relative mt-2 rounded-md border border-zinc-200 bg-zinc-50 px-3 py-2">
              <button
                type="button"
                className="absolute right-1 top-1 rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
                aria-label="Copy thinking"
                title="Copy thinking"
                onClick={() => copyToClipboard(msg.reasoning_content ?? "")}
              >
                <Copy className="h-3.5 w-3.5" />
              </button>
              <summary className="cursor-pointer select-none pr-10 text-xs text-zinc-600">
                thinking
                {thinkingDurationsMs[msg.id] != null ? (
                  <span className="ml-2 text-zinc-500">({fmtDurationCompact(thinkingDurationsMs[msg.id]!)})</span>
                ) : null}
              </summary>
              <div className="mt-2 whitespace-pre-wrap text-xs italic text-zinc-700">
                {msg.reasoning_content}
              </div>
            </details>
          ) : null}
          {renderToolCalls(msg.tool_calls ?? [], toolDurationsMs)}

          <div className="mt-2 border-t border-zinc-200 pt-1">
            <div className="flex items-center gap-1">
              <button
                type="button"
                className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
                aria-label="Copy message"
                title="Copy message"
                onClick={() => copyToClipboard(msg.content ?? "")}
              >
                <Copy className="h-4 w-4" />
              </button>
              <button
                type="button"
                disabled={!onFork}
                className={[
                  "rounded-md p-1 text-zinc-500",
                  onFork ? "hover:bg-zinc-100" : "cursor-not-allowed opacity-40",
                ].join(" ")}
                aria-label="Fork session"
                title="Fork session from here"
                onClick={() => onFork?.(msg.id)}
              >
                <GitFork className="h-4 w-4" />
              </button>
              <button
                type="button"
                className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
                aria-label="Menu"
                title="Menu"
              >
                <MoreHorizontal className="h-4 w-4" />
              </button>
              {assistantDurationsMs[msg.id] != null ? (
                <span className="ml-auto text-xs text-zinc-500">
                  {fmtDurationCompact(assistantDurationsMs[msg.id]!)}
                </span>
              ) : null}
            </div>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex justify-start">
      <details className="relative w-full max-w-[78%] rounded-2xl border border-zinc-200 bg-zinc-50 px-4 py-2">
        <button
          type="button"
          className="absolute right-2 top-2 rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
          aria-label="Copy tool result"
          title="Copy tool result"
          onClick={() => copyToClipboard(msg.content ?? "")}
        >
          <Copy className="h-3.5 w-3.5" />
        </button>
        <summary className="cursor-pointer select-none pr-10 text-xs text-zinc-700">
          tool_result: <span className="font-mono">{msg.tool_call_id}</span>
          {toolDurationsMs[msg.tool_call_id] != null ? (
            <span className="ml-2 text-zinc-500">({fmtDurationCompact(toolDurationsMs[msg.tool_call_id]!)})</span>
          ) : null}
        </summary>
        <CodeBlock className="mt-2" code={msg.content} lang="json" copyButton={false} />
      </details>
    </div>
  );
}

function EmptyState() {
  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="text-center">
        <div className="text-xl font-semibold text-zinc-900">Let&apos;s build</div>
        <div className="mt-1 text-sm text-zinc-600">Start typing below to create a session</div>
      </div>
    </div>
  );
}

export default function App() {
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [pinnedSessionIds, setPinnedSessionIds] = useState<string[]>(() =>
    loadPinnedSessionIds(),
  );
  const [pinnedWorkspaceRoots, setPinnedWorkspaceRoots] = useState<string[]>(() =>
    loadPinnedWorkspaceRoots(),
  );
  const [sidebarOpen, setSidebarOpen] = useState<boolean>(() => loadSidebarOpen());
  const [workspacesPaneOpen, setWorkspacesPaneOpen] = useState<boolean>(() =>
    loadSidebarSectionOpen(WORKSPACES_OPEN_KEY, true),
  );
  const [sessionsPaneOpen, setSessionsPaneOpen] = useState<boolean>(() =>
    loadSidebarSectionOpen(SESSIONS_OPEN_KEY, true),
  );
  const [sessionsVisible, setSessionsVisible] = useState<number>(LIST_PAGE_SIZE);
  const [workspacesVisible, setWorkspacesVisible] = useState<number>(LIST_PAGE_SIZE);
  const [expandedWorkspaces, setExpandedWorkspaces] = useState<Record<string, boolean>>({});
  const route = useRoute();
  const selectedId = route.name === "session" ? route.sessionId : null;
  const [session, setSession] = useState<Session | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [pending, setPending] = useState<PendingMessage[]>([]);
  const [stream, setStream] = useState<StreamState>({
    thinking: "",
    assistant: "",
    assistantStarted: false,
    toolCalls: [],
  });
  const [toolDurationsMs, setToolDurationsMs] = useState<Record<string, number>>({});
  const [assistantDurationsMs, setAssistantDurationsMs] = useState<Record<string, number>>({});
  const [thinkingDurationsMs, setThinkingDurationsMs] = useState<Record<string, number>>({});
  const [clockNowMs, setClockNowMs] = useState<number>(() => monotonicNowMs());
  const toolStartsRef = useRef<Record<string, { name: string; startedAt: number }>>({});
  const thinkingStartedAtRef = useRef<number | null>(null);
  const assistantStartedAtRef = useRef<number | null>(null);
  const nextThinkingDurationMsRef = useRef<number | null>(null);
  const [isAtBottom, setIsAtBottom] = useState(true);

  const [composerText, setComposerText] = useState("");
  const [savingConfig, setSavingConfig] = useState(false);
  const [configYaml, setConfigYaml] = useState("");
  const [configPath, setConfigPath] = useState("");

  const [skillsOpen, setSkillsOpen] = useState(false);
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [skillsDefaultEnable, setSkillsDefaultEnable] = useState(true);
  const [skillsOverrides, setSkillsOverrides] = useState<Record<string, boolean>>({});
  const [skillsSaving, setSkillsSaving] = useState(false);
  const [mcpOpen, setMcpOpen] = useState(false);
  const [mcpSaving, setMcpSaving] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);

  const [sessionMenu, setSessionMenu] = useState<{
    sessionId: string;
    x: number;
    y: number;
  } | null>(null);
  const sessionMenuRef = useRef<HTMLDivElement | null>(null);

  const [deleteConfirm, setDeleteConfirm] = useState<{
    sessionId: string;
  } | null>(null);

  const [workspaceDeleteConfirm, setWorkspaceDeleteConfirm] = useState<{
    workspaceRoot: string;
  } | null>(null);

  const [workspaceMenu, setWorkspaceMenu] = useState<{
    workspaceRoot: string;
    x: number;
    y: number;
  } | null>(null);
  const workspaceMenuRef = useRef<HTMLDivElement | null>(null);

  const [workspaceCreateOpen, setWorkspaceCreateOpen] = useState(false);
  const [workspacePickerPath, setWorkspacePickerPath] = useState("");
  const [workspaceCreateSaving, setWorkspaceCreateSaving] = useState(false);

  const [addFolderOpen, setAddFolderOpen] = useState(false);
  const [extraFolderPickerPath, setExtraFolderPickerPath] = useState("");
  const [extraFolderSaving, setExtraFolderSaving] = useState(false);

  const [authError, setAuthError] = useState<string | null>(null);
  const [alerts, setAlerts] = useState<AlertItem[]>([]);
  const alertTimersRef = useRef<Record<string, number>>({});
  const seenMermaidAlertKeysRef = useRef<Set<string>>(new Set());

  const wsRef = useRef<WebSocket | null>(null);
  const chatScrollRef = useRef<HTMLDivElement | null>(null);
  const chatEndRef = useRef<HTMLDivElement | null>(null);
  const composerRef = useRef<HTMLTextAreaElement | null>(null);
  const selectedIdRef = useRef<string | null>(selectedId);
  const skipNextFetchRef = useRef<string | null>(null);
  const isAtBottomRef = useRef(true);

  const sortedSessions = useMemo(
    () => sortSessions(sessions, pinnedSessionIds),
    [sessions, pinnedSessionIds],
  );
  const tmpSessions = useMemo(
    () =>
      sortedSessions.filter((s) =>
        isTmpWorkspaceRoot(s.settings.workspace_root ?? ""),
      ),
    [sortedSessions],
  );
  const visibleSessions = useMemo(
    () => tmpSessions.slice(0, sessionsVisible),
    [tmpSessions, sessionsVisible],
  );
  const workspaceGroups = useMemo(() => {
    const byRoot = new Map<string, SessionSummary[]>();
    for (const s of sortedSessions) {
      const root = s.settings.workspace_root ?? "";
      if (!root.trim()) continue;
      const arr = byRoot.get(root) ?? [];
      arr.push(s);
      byRoot.set(root, arr);
    }
    const groups = Array.from(byRoot.entries()).map(([root, items]) => ({
      root,
      sessions: items,
      isTmp: isTmpWorkspaceRoot(root),
    }));
    // Web default sessions use tmp workspaces; keep them out of the Workspaces pane.
    // They remain accessible via the Sessions pane.
    const nonTmp = groups.filter((g) => !g.isTmp);
    const pinnedRank = new Map(pinnedWorkspaceRoots.map((r, idx) => [r, idx]));
    nonTmp.sort((a, b) => {
      const aPinned = pinnedRank.has(a.root);
      const bPinned = pinnedRank.has(b.root);
      if (aPinned !== bPinned) return aPinned ? -1 : 1;
      if (aPinned && bPinned) {
        return (pinnedRank.get(a.root) ?? 0) - (pinnedRank.get(b.root) ?? 0);
      }
      return a.root.localeCompare(b.root);
    });
    return nonTmp;
  }, [sortedSessions, pinnedWorkspaceRoots]);
  const visibleWorkspaceGroups = useMemo(
    () => workspaceGroups.slice(0, workspacesVisible),
    [workspaceGroups, workspacesVisible],
  );
  const deleteSessionSummary = deleteConfirm
    ? sortedSessions.find((s) => s.id === deleteConfirm.sessionId) ?? null
    : null;
  const canLoadMoreSessions = visibleSessions.length < tmpSessions.length;
  const canLoadMoreWorkspaces =
    visibleWorkspaceGroups.length < workspaceGroups.length;

  function handleApiError(err: unknown) {
    if (err instanceof ApiError && err.status === 401) {
      setAuthError("Unauthorized. Re-open the URL printed by `kiliax serve start`.");
      setAlerts([]);
      return;
    }
    if (err instanceof ApiError) {
      const subtitle = err.code ? `HTTP ${err.status} • ${err.code}` : `HTTP ${err.status}`;
      const details =
        err.details != null ? err.details : err.bodyText ? { raw_body: err.bodyText } : undefined;
      pushAlert({
        id: newAlertId("api"),
        title: "Request failed",
        subtitle,
        message: err.message,
        traceId: err.traceId,
        details,
      });
    } else {
      const message = err instanceof Error ? err.message : String(err);
      pushAlert({
        id: newAlertId("ui"),
        title: "Unexpected error",
        message,
      });
    }
    // eslint-disable-next-line no-console
    console.error(err);
  }

  function closeAlert(id: string) {
    const timerId = alertTimersRef.current[id];
    if (timerId != null) {
      window.clearTimeout(timerId);
      delete alertTimersRef.current[id];
    }
    setAlerts((prev) => prev.filter((a) => a.id !== id));
  }

  function pruneAlerts(items: AlertItem[]): AlertItem[] {
    const autoClose = items.filter((a) => a.autoCloseMs != null);
    if (autoClose.length <= 3) return items;
    const keep = new Set(autoClose.slice(-3).map((a) => a.id));
    return items.filter((a) => a.autoCloseMs == null || keep.has(a.id));
  }

  function pushAlert(alert: AlertItem) {
    setAlerts((prev) => pruneAlerts([...prev, alert]));
  }

  function handleMermaidError(info: MermaidErrorInfo) {
    const key = (info.key ?? "").trim();
    if (key && seenMermaidAlertKeysRef.current.has(key)) return;
    if (key) seenMermaidAlertKeysRef.current.add(key);

    pushAlert({
      id: newAlertId("mermaid"),
      title: "Mermaid error",
      message: "Mermaid diagram failed to render.",
      details: { key: key || undefined, error: info.message },
      autoCloseMs: 6000,
    });
  }

  useEffect(() => {
    const activeIds = new Set(alerts.map((a) => a.id));
    for (const [id, timerId] of Object.entries(alertTimersRef.current)) {
      if (activeIds.has(id)) continue;
      window.clearTimeout(timerId);
      delete alertTimersRef.current[id];
    }

    for (const a of alerts) {
      const autoCloseMs = a.autoCloseMs;
      if (autoCloseMs == null) continue;
      if (alertTimersRef.current[a.id] != null) continue;
      alertTimersRef.current[a.id] = window.setTimeout(() => {
        setAlerts((prev) => prev.filter((item) => item.id !== a.id));
      }, autoCloseMs);
    }
  }, [alerts]);

  useEffect(() => {
    return () => {
      for (const timerId of Object.values(alertTimersRef.current)) {
        window.clearTimeout(timerId);
      }
      alertTimersRef.current = {};
    };
  }, []);

  async function refreshCapabilities() {
    try {
      const caps = await api.getCapabilities();
      setCapabilities(caps);
      setAuthError(null);
    } catch (err) {
      handleApiError(err);
    }
  }

  async function refreshSessions() {
    try {
      const list = await api.listSessions();
      setSessions(list.items);
      setAuthError(null);
    } catch (err) {
      handleApiError(err);
    }
  }

  function selectSession(sessionId: string) {
    navigate(hrefToSession(sessionId));
  }

  function updateIsAtBottom() {
    const el = chatScrollRef.current;
    if (!el) return;
    const thresholdPx = 96;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    const next = distance <= thresholdPx;
    if (next === isAtBottomRef.current) return;
    isAtBottomRef.current = next;
    setIsAtBottom(next);
  }

  function scrollToBottom(behavior: ScrollBehavior = "auto") {
    chatEndRef.current?.scrollIntoView({ behavior });
  }

  async function fetchSession(sessionId: string) {
    try {
      const s = await api.getSession(sessionId);
      if (selectedIdRef.current !== sessionId) return;
      setSession(s);

      const msgs = await api.getMessages(sessionId, 200);
      if (selectedIdRef.current !== sessionId) return;
      setMessages(msgs.items);
      setPending((p) => p.filter((m) => m.sessionId !== sessionId));
      setStream({
        thinking: "",
        assistant: "",
        assistantStarted: false,
        toolCalls: [],
      });
      setToolDurationsMs({});
      setAssistantDurationsMs({});
      setThinkingDurationsMs({});
      toolStartsRef.current = {};
      thinkingStartedAtRef.current = null;
      assistantStartedAtRef.current = null;
      nextThinkingDurationMsRef.current = null;

      connectWs(sessionId, s.status.last_event_id);
    } catch (err) {
      handleApiError(err);
    }
  }

  function connectWs(sessionId: string, afterEventId: number) {
    if (wsRef.current) {
      wsRef.current.close();
      wsRef.current = null;
    }

    const url = wsUrl(
      `/v1/sessions/${sessionId}/events/ws?after_event_id=${afterEventId}`,
    );
    const ws = new WebSocket(url);
    wsRef.current = ws;

    ws.onmessage = (ev) => {
      try {
        const msg = JSON.parse(ev.data as string);
        handleEvent(msg);
      } catch (e) {
        // eslint-disable-next-line no-console
        console.error("ws parse error", e);
      }
    };
    ws.onerror = () => {
      // eslint-disable-next-line no-console
      console.error("ws error");
    };
  }

  async function handleEvent(ev: any) {
    const type = ev?.type ?? ev?.event_type;
    if (!type) return;
    const now = monotonicNowMs();

    if (type === "step_start") {
      thinkingStartedAtRef.current = now;
      assistantStartedAtRef.current = null;
      nextThinkingDurationMsRef.current = null;
      return;
    }

    if (type === "assistant_thinking_delta") {
      const delta = ev?.data?.delta ?? "";
      if (!delta) return;
      if (thinkingStartedAtRef.current == null && assistantStartedAtRef.current == null) {
        thinkingStartedAtRef.current = now;
      }
      setStream((s) => {
        if (s.assistantStarted) return s;
        return { ...s, thinking: s.thinking + delta };
      });
      return;
    }

    if (type === "assistant_delta") {
      const delta = ev?.data?.delta ?? "";
      if (!delta) return;
      if (assistantStartedAtRef.current == null) {
        assistantStartedAtRef.current = now;
        if (thinkingStartedAtRef.current != null && nextThinkingDurationMsRef.current == null) {
          nextThinkingDurationMsRef.current = now - thinkingStartedAtRef.current;
        }
      }
      setStream((s) => ({
        ...s,
        assistantStarted: true,
        assistant: s.assistant + delta,
      }));
      return;
    }

    if (type === "tool_call") {
      const call = ev?.data?.call;
      if (!call?.name) return;
      const callId = String(call.id ?? "");
      toolStartsRef.current[callId] = {
        name: String(call.name),
        startedAt: now,
      };
      setStream((s) => ({
        ...s,
        toolCalls: [
          ...s.toolCalls,
          { id: callId, name: String(call.name), arguments: String(call.arguments ?? "") },
        ],
      }));
      return;
    }

    if (type === "tool_result") {
      const msg = ev?.data?.message;
      if (!msg?.role) return;
      const toolCallId = String(msg?.tool_call_id ?? "");
      const startedAt = toolStartsRef.current[toolCallId]?.startedAt ?? null;
      if (startedAt != null && toolCallId) {
        const elapsed = now - startedAt;
        setToolDurationsMs((prev) => {
          if (prev[toolCallId] != null) return prev;
          return { ...prev, [toolCallId]: elapsed };
        });
      }
      setMessages((m) => [...m, msg as Message]);
      return;
    }

    if (type === "assistant_message") {
      const msg = ev?.data?.message;
      if (!msg?.role) return;
      const messageId = String(msg?.id ?? "");
      if (assistantStartedAtRef.current != null && messageId) {
        const elapsed = now - assistantStartedAtRef.current!;
        setAssistantDurationsMs((prev) => {
          if (prev[messageId] != null) return prev;
          return { ...prev, [messageId]: elapsed };
        });
      }
      const thinkingElapsed =
        nextThinkingDurationMsRef.current != null
          ? nextThinkingDurationMsRef.current
          : thinkingStartedAtRef.current != null
            ? now - thinkingStartedAtRef.current
            : null;
      if (thinkingElapsed != null && messageId) {
        setThinkingDurationsMs((prev) => {
          if (prev[messageId] != null) return prev;
          return { ...prev, [messageId]: thinkingElapsed };
        });
      }
      setMessages((m) => [...m, msg as Message]);
      setStream({ thinking: "", assistant: "", assistantStarted: false, toolCalls: [] });
      assistantStartedAtRef.current = null;
      nextThinkingDurationMsRef.current = null;
      return;
    }

    if (type === "session_settings_changed") {
      if (selectedId) {
        await fetchSession(selectedId);
      }
      return;
    }

    if (type === "run_error") {
      const run = ev?.data?.run ?? null;
      const diagnostics = ev?.data?.diagnostics ?? null;
      const code = run?.error?.code ?? diagnostics?.code ?? "error";
      const message = run?.error?.message ?? "Run failed";
      const traceId = diagnostics?.trace_id ?? undefined;
      const step =
        typeof diagnostics?.step === "number" ? String(diagnostics.step) : undefined;
      const subtitleParts = [`code: ${code}`];
      if (step) subtitleParts.push(`step: ${step}`);
      pushAlert({
        id: newAlertId("run"),
        title: "Run failed",
        subtitle: subtitleParts.join(" • "),
        message,
        traceId,
        details: { diagnostics, run },
      });
    }

    if (type === "run_done" || type === "run_error" || type === "run_cancelled") {
      thinkingStartedAtRef.current = null;
      assistantStartedAtRef.current = null;
      nextThinkingDurationMsRef.current = null;
      if (selectedId) {
        await fetchSession(selectedId);
      }
      return;
    }
  }

  async function onNewSession() {
    try {
      const s = await api.createSession();
      await refreshSessions();
      selectSession(s.id);
    } catch (err) {
      handleApiError(err);
    }
  }

  async function onNewSessionInWorkspace(workspaceRoot: string) {
    try {
      const s = await api.createSession({ settings: { workspace_root: workspaceRoot } });
      await refreshSessions();
      selectSession(s.id);
    } catch (err) {
      handleApiError(err);
    }
  }

  async function deleteWorkspace(workspaceRoot: string) {
    const ids = sessions
      .filter((s) => (s.settings.workspace_root ?? "") === workspaceRoot)
      .map((s) => s.id);
    try {
      for (const id of ids) {
        await api.deleteSession(id);
      }
      setPinnedWorkspaceRoots((prev) => prev.filter((r) => r !== workspaceRoot));
      setPinnedSessionIds((prev) => prev.filter((id) => !ids.includes(id)));
      if (selectedIdRef.current && ids.includes(selectedIdRef.current)) {
        navigate("/", { replace: true });
      }
      await refreshSessions();
    } catch (err) {
      handleApiError(err);
    }
  }

  async function createSessionWithWorkspaceRoot(path: string) {
    const trimmed = path.trim();
    if (!trimmed) return;
    setWorkspaceCreateSaving(true);
    try {
      const s = await api.createSession({ settings: { workspace_root: trimmed } });
      await refreshSessions();
      setWorkspaceCreateOpen(false);
      setWorkspacePickerPath("");
      selectSession(s.id);
    } catch (err) {
      handleApiError(err);
    } finally {
      setWorkspaceCreateSaving(false);
    }
  }

  async function onSend() {
    const text = composerText.trim();
    if (!text) return;

    const baseEventId = selectedSummary?.status.last_event_id ?? 0;
    const localMessageId = `local_${Date.now().toString(16)}_${Math.random()
      .toString(16)
      .slice(2)}`;
    const localCreatedAt = new Date().toISOString();
    setComposerText("");

    let didAppendLocalMessage = false;
    let createdRunId: string | null = null;
    try {
      let sessionId = selectedId;

      if (!sessionId) {
        const s = await api.createSession();
        sessionId = s.id;
        skipNextFetchRef.current = sessionId;
        selectSession(sessionId);
        selectedIdRef.current = sessionId;
        await fetchSession(sessionId);
      }

      // Optimistic user message to keep ordering stable before tool output arrives.
      setMessages((m) => [
        ...m,
        {
          role: "user",
          id: localMessageId,
          created_at: localCreatedAt,
          content: text,
        },
      ]);
      didAppendLocalMessage = true;

      const run: any = await api.createRun(sessionId, {
        input: { type: "text", text },
      });
      createdRunId = String(run?.id ?? "").trim() || null;
      setPending((p) => [
        ...p,
        {
          sessionId,
          runId: createdRunId ?? "",
          baseEventId,
          createdAt: new Date().toISOString(),
          content: text,
        },
      ]);
      await refreshSessions();
    } catch (err) {
      if (createdRunId == null) {
        setComposerText(text);
        if (didAppendLocalMessage) {
          setMessages((m) => m.filter((msg) => msg.id !== localMessageId));
        }
      }
      handleApiError(err);
    }
  }

  async function deleteSession(sessionId: string) {
    try {
      await api.deleteSession(sessionId);
      setPinnedSessionIds((prev) => prev.filter((id) => id !== sessionId));
      if (selectedIdRef.current === sessionId) {
        navigate("/", { replace: true });
      }
      await refreshSessions();
    } catch (err) {
      handleApiError(err);
    }
  }

  function togglePinnedSession(sessionId: string) {
    setPinnedSessionIds((prev) => {
      const exists = prev.includes(sessionId);
      const next = exists ? prev.filter((id) => id !== sessionId) : [sessionId, ...prev];
      return next;
    });
  }

  function togglePinnedWorkspace(workspaceRoot: string) {
    setPinnedWorkspaceRoots((prev) => {
      const exists = prev.includes(workspaceRoot);
      const next = exists
        ? prev.filter((r) => r !== workspaceRoot)
        : [workspaceRoot, ...prev];
      return next;
    });
  }

  async function openSkills() {
    try {
      const [skillsRes, cfgRes] = await Promise.all([
        api.listGlobalSkills(),
        api.getConfigSkills(),
      ]);
      setSkills(skillsRes.items);
      setSkillsDefaultEnable(cfgRes.default_enable ?? true);
      const nextOverrides: Record<string, boolean> = {};
      for (const s of cfgRes.skills ?? []) {
        if (!s?.id) continue;
        nextOverrides[s.id] = Boolean(s.enable);
      }
      setSkillsOverrides(nextOverrides);
      setSkillsOpen(true);
    } catch (err) {
      handleApiError(err);
    }
  }

  async function openSettings() {
    try {
      const cfg = await api.getConfig();
      setConfigYaml(cfg.yaml);
      setConfigPath(cfg.path);
      setSettingsOpen(true);
    } catch (err) {
      handleApiError(err);
    }
  }

  async function openMcp() {
    await refreshCapabilities();
    setMcpOpen(true);
  }

  async function saveConfig() {
    setSavingConfig(true);
    try {
      await api.putConfig({ yaml: configYaml });
      setSettingsOpen(false);
      await refreshCapabilities();
      await refreshSessions();
      if (selectedId) await fetchSession(selectedId);
    } catch (err) {
      handleApiError(err);
    } finally {
      setSavingConfig(false);
    }
  }

  async function patchSession(patch: any) {
    if (!selectedId) return;
    try {
      const s = await api.patchSessionSettings(selectedId, patch);
      setSession(s);
      await refreshSessions();
    } catch (err) {
      handleApiError(err);
    }
  }

  async function addExtraFolder(path: string) {
    if (!session || !selectedId) return;
    const trimmed = path.trim();
    if (!trimmed) return;
    const existing = session.settings.extra_workspace_roots ?? [];
    const next = Array.from(new Set([...existing, trimmed]));
    await patchSession({ extra_workspace_roots: next });
  }

  async function removeExtraFolder(path: string) {
    if (!session || !selectedId) return;
    const next = (session.settings.extra_workspace_roots ?? []).filter((p) => p !== path);
    await patchSession({ extra_workspace_roots: next });
  }

  async function openWorkspace(target: "vscode" | "file_manager" | "terminal") {
    if (!selectedId) return;
    try {
      await api.openWorkspace(selectedId, target);
    } catch (err) {
      handleApiError(err);
    }
  }

  async function forkSessionFromAssistant(assistantMessageId: string) {
    if (!selectedId) return;
    try {
      const res: any = await api.forkSession(selectedId, assistantMessageId);
      const newId = res?.session?.id;
      if (typeof newId !== "string" || !newId.trim()) {
        throw new Error("Invalid fork response");
      }
      await refreshSessions();
      selectSession(newId);
    } catch (err) {
      handleApiError(err);
    }
  }

  useEffect(() => {
    let cancelled = false;
    async function init() {
      const params = new URLSearchParams(window.location.search);
      const token = params.get("token")?.trim() ?? "";
      if (token) {
        try {
          await fetch(`/v1/capabilities?token=${encodeURIComponent(token)}`);
        } catch (err) {
          handleApiError(err);
        }
        if (cancelled) return;
        const url = new URL(window.location.href);
        url.searchParams.delete("token");
        window.history.replaceState({}, "", url.pathname + url.search + url.hash);
      }

      refreshCapabilities();
      refreshSessions();
    }

    init();
    const t = window.setInterval(() => {
      refreshSessions();
    }, 1000);
    return () => {
      cancelled = true;
      window.clearInterval(t);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    savePinnedSessionIds(pinnedSessionIds);
  }, [pinnedSessionIds]);

  useEffect(() => {
    savePinnedWorkspaceRoots(pinnedWorkspaceRoots);
  }, [pinnedWorkspaceRoots]);

  useEffect(() => {
    saveSidebarOpen(sidebarOpen);
  }, [sidebarOpen]);

  useEffect(() => {
    saveSidebarSectionOpen(WORKSPACES_OPEN_KEY, workspacesPaneOpen);
  }, [workspacesPaneOpen]);

  useEffect(() => {
    saveSidebarSectionOpen(SESSIONS_OPEN_KEY, sessionsPaneOpen);
  }, [sessionsPaneOpen]);

  useEffect(() => {
    composerRef.current?.focus();
  }, []);

  useEffect(() => {
    const el = composerRef.current;
    if (!el) return;
    const minPx = 44;
    const maxPx = 240;
    el.style.height = "auto";
    const next = Math.min(maxPx, Math.max(minPx, el.scrollHeight));
    el.style.height = `${next}px`;
  }, [composerText]);

  useEffect(() => {
    setPinnedSessionIds((prev) => {
      if (!prev.length) return prev;
      const ids = new Set(sessions.map((s) => s.id));
      const next = prev.filter((id) => ids.has(id));
      return next.length === prev.length ? prev : next;
    });
  }, [sessions]);

  useEffect(() => {
    setPending((prev) => {
      if (!prev.length) return prev;
      return prev.filter((p) => {
        const summary = sessions.find((s) => s.id === p.sessionId);
        if (!summary) return false;

        const hasWork =
          summary.status.active_run_id != null ||
          summary.status.queue_len > 0 ||
          summary.status.run_state === "running" ||
          summary.status.run_state === "tooling";
        if (hasWork) return true;

        // Keep the pending entry only while the sessions list hasn't reflected it yet.
        return summary.status.last_event_id <= p.baseEventId;
      });
    });
  }, [sessions]);

  useEffect(() => {
    setPinnedWorkspaceRoots((prev) => {
      if (!prev.length) return prev;
      const roots = new Set(workspaceGroups.map((g) => g.root));
      const next = prev.filter((r) => roots.has(r));
      return next.length === prev.length ? prev : next;
    });
  }, [workspaceGroups]);

  useEffect(() => {
    if (!sessionMenu) return;
    const onPointerDown = (e: PointerEvent) => {
      const el = sessionMenuRef.current;
      if (!el) return;
      if (e.target instanceof Node && el.contains(e.target)) return;
      setSessionMenu(null);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setSessionMenu(null);
    };
    const onScrollOrResize = () => setSessionMenu(null);

    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("scroll", onScrollOrResize, true);
    window.addEventListener("resize", onScrollOrResize);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("scroll", onScrollOrResize, true);
      window.removeEventListener("resize", onScrollOrResize);
    };
  }, [sessionMenu]);

  useEffect(() => {
    if (!workspaceMenu) return;
    const onPointerDown = (e: PointerEvent) => {
      const el = workspaceMenuRef.current;
      if (!el) return;
      if (e.target instanceof Node && el.contains(e.target)) return;
      setWorkspaceMenu(null);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setWorkspaceMenu(null);
    };
    const onScrollOrResize = () => setWorkspaceMenu(null);

    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("scroll", onScrollOrResize, true);
    window.addEventListener("resize", onScrollOrResize);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("scroll", onScrollOrResize, true);
      window.removeEventListener("resize", onScrollOrResize);
    };
  }, [workspaceMenu]);

  useEffect(() => {
    selectedIdRef.current = selectedId;
    isAtBottomRef.current = true;
    setIsAtBottom(true);
    if (!selectedId) {
      setSession(null);
      setMessages([]);
      setStream({
        thinking: "",
        assistant: "",
        assistantStarted: false,
        toolCalls: [],
      });
      if (wsRef.current) {
        wsRef.current.close();
        wsRef.current = null;
      }
      setToolDurationsMs({});
      setAssistantDurationsMs({});
      setThinkingDurationsMs({});
      toolStartsRef.current = {};
      thinkingStartedAtRef.current = null;
      assistantStartedAtRef.current = null;
      nextThinkingDurationMsRef.current = null;
      return;
    }
    if (skipNextFetchRef.current === selectedId) {
      skipNextFetchRef.current = null;
      return;
    }
    fetchSession(selectedId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId]);

  const pendingForSelected = useMemo(
    () => pending.filter((p) => p.sessionId === selectedId),
    [pending, selectedId],
  );

  useEffect(() => {
    if (!isAtBottomRef.current) return;
    scrollToBottom("auto");
  }, [messages, pendingForSelected.length, stream.assistant, stream.thinking, stream.toolCalls.length]);

  const selectedSummary = sortedSessions.find((s) => s.id === selectedId) ?? null;
  const selectedBadge = selectedSummary ? statusBadge(selectedSummary) : null;

  const composerHasText = composerText.trim().length > 0;
  const lastPendingRun = pendingForSelected[pendingForSelected.length - 1] ?? null;
  const sessionHasWork = Boolean(
    selectedSummary &&
      (selectedSummary.status.active_run_id != null ||
        selectedSummary.status.queue_len > 0 ||
        selectedSummary.status.run_state === "running" ||
        selectedSummary.status.run_state === "tooling"),
  );
  const pendingCanCancel =
    Boolean(lastPendingRun?.runId) &&
    (selectedSummary == null ||
      selectedSummary.status.last_event_id <= lastPendingRun.baseEventId);

  const cancellableRunId =
    selectedSummary?.status.active_run_id ?? lastPendingRun?.runId ?? null;
  const showInterrupt =
    Boolean(selectedId) &&
    !composerHasText &&
    Boolean(cancellableRunId) &&
    (sessionHasWork || pendingCanCancel);

  const agentOptions = capabilities?.agents ?? [];
  const modelOptions = capabilities?.models ?? [];
  const streamHasThinking = stream.thinking.length > 0;
  const streamHasAssistant = stream.assistant.length > 0;
  const streamToolCallCount = stream.toolCalls.length;
  const liveThinkingElapsedMs =
    nextThinkingDurationMsRef.current ??
    (thinkingStartedAtRef.current != null ? clockNowMs - thinkingStartedAtRef.current : null);
  const liveAssistantElapsedMs =
    assistantStartedAtRef.current != null ? clockNowMs - assistantStartedAtRef.current : null;

  useEffect(() => {
    const active = Boolean(
      selectedId &&
        (sessionHasWork ||
          streamHasThinking ||
          streamHasAssistant ||
          streamToolCallCount > 0),
    );
    if (!active) return;
    const t = window.setInterval(() => setClockNowMs(monotonicNowMs()), 250);
    return () => window.clearInterval(t);
  }, [selectedId, sessionHasWork, streamHasThinking, streamHasAssistant, streamToolCallCount]);

  if (authError) {
    return (
      <div className="h-dvh w-full bg-zinc-50 text-zinc-900">
        <div className="flex h-full items-center justify-center p-6">
          <div className="w-full max-w-md rounded-lg border border-zinc-200 bg-white p-6">
            <div className="text-base font-semibold">Unauthorized</div>
            <div className="mt-2 text-sm text-zinc-600">{authError}</div>
            <div className="mt-4 flex justify-end">
              <Button variant="outline" onClick={() => window.location.reload()}>
                Reload
              </Button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="h-dvh w-full bg-zinc-50 text-zinc-900">
      <AlertStack items={alerts} onClose={closeAlert} />
      <div className="flex h-full">
        {sidebarOpen ? (
          <aside className="flex w-[280px] flex-col border-r border-zinc-200 bg-zinc-50">
            <div className="space-y-1 p-3">
              <Button variant="ghost" className="w-full justify-start gap-2" onClick={onNewSession}>
                <Plus className="h-4 w-4 text-violet-600" />
                New Session
              </Button>
              <Button
                variant="ghost"
                className="w-full justify-start gap-2"
                onClick={openSkills}
              >
                <Sparkles className="h-4 w-4 text-amber-600" />
                Skills
              </Button>
              <Button
                variant="ghost"
                className="w-full justify-start gap-2"
                onClick={openMcp}
              >
                <Plug className="h-4 w-4 text-emerald-600" />
                MCP
              </Button>
            </div>

            <Separator />

            <div className="min-h-0 flex-1 p-2">
              <div className="flex h-full flex-col gap-2">
                <div className="flex items-center justify-between px-2">
                  <button
                    className="flex items-center gap-1 text-xs font-medium text-zinc-500 hover:text-zinc-700"
                    onClick={() => setWorkspacesPaneOpen((v) => !v)}
                  >
                    {workspacesPaneOpen ? (
                      <ChevronDown className="h-3.5 w-3.5" />
                    ) : (
                      <ChevronRight className="h-3.5 w-3.5" />
                    )}
                    Workspaces
                  </button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7"
                    aria-label="Add workspace folder"
                    onClick={() => {
                      setWorkspacePickerPath("");
                      setWorkspaceCreateOpen(true);
                    }}
                  >
                    <FolderPlus className="h-4 w-4 text-violet-600" />
                  </Button>
                </div>

                {workspacesPaneOpen ? (
                  <div className="min-h-0 flex-1 overflow-auto px-1">
                    <div className="space-y-1">
                      {visibleWorkspaceGroups.map((g) => {
                        const expanded = Boolean(expandedWorkspaces[g.root]);
                        const pinnedWorkspace = pinnedWorkspaceRoots.includes(g.root);
                        return (
                          <div key={g.root} className="rounded-md">
                            <div className="group flex items-center gap-1 rounded-md px-2 py-1.5 hover:bg-white/70">
                              <button
                                className="flex min-w-0 flex-1 items-center gap-1 text-left"
                                onClick={() =>
                                  setExpandedWorkspaces((prev) => ({
                                    ...prev,
                                    [g.root]: !Boolean(prev[g.root]),
                                  }))
                                }
                              >
                                {expanded ? (
                                  <ChevronDown className="h-3.5 w-3.5 shrink-0 text-zinc-500" />
                                ) : (
                                  <ChevronRight className="h-3.5 w-3.5 shrink-0 text-zinc-500" />
                                )}
                                {pinnedWorkspace ? (
                                  <Pin className="h-3.5 w-3.5 shrink-0 text-violet-600" />
                                ) : null}
                                <div
                                  className="min-w-0 truncate text-xs font-medium text-zinc-800"
                                  title={g.root}
                                >
                                  {workspaceBasename(g.root)}
                                </div>
                                <div className="shrink-0 text-[10px] text-zinc-500">
                                  ({g.sessions.length})
                                </div>
                              </button>

                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-7 w-7"
                                aria-label="Workspace actions"
                                onClick={(e) => {
                                  e.preventDefault();
                                  e.stopPropagation();
                                  const rect = (
                                    e.currentTarget as HTMLButtonElement
                                  ).getBoundingClientRect();
                                  setWorkspaceMenu((prev) => {
                                    if (prev?.workspaceRoot === g.root) return null;
                                    return { workspaceRoot: g.root, x: rect.right, y: rect.bottom };
                                  });
                                }}
                              >
                                <MoreHorizontal className="h-4 w-4 text-zinc-500" />
                              </Button>

                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-7 w-7"
                                aria-label="New session in workspace"
                                onClick={(e) => {
                                  e.preventDefault();
                                  e.stopPropagation();
                                  onNewSessionInWorkspace(g.root);
                                }}
                              >
                                <Plus className="h-4 w-4 text-violet-600" />
                              </Button>
                            </div>

                            {expanded ? (
                              <div className="mt-1 space-y-1 pl-5">
                                {g.sessions.map((s) => {
                                  const badge = statusBadge(s);
                                  const active = s.id === selectedId;
                                  const pinned = pinnedSessionIds.includes(s.id);
                                  return (
                                    <button
                                      key={s.id}
                                      onClick={() => selectSession(s.id)}
                                      className={[
                                        "flex w-full items-center justify-between gap-2 rounded-md px-2 py-1 text-left text-xs",
                                        active
                                          ? "bg-white shadow-sm"
                                          : "hover:bg-white/70",
                                      ].join(" ")}
                                    >
                                      <div className="min-w-0 flex items-center gap-1 text-zinc-800">
                                        {pinned ? (
                                          <Pin className="h-3.5 w-3.5 shrink-0 text-violet-600" />
                                        ) : null}
                                        <div className="truncate">{s.title || s.id}</div>
                                      </div>
                                      <Badge variant={badge.variant}>{badge.label}</Badge>
                                    </button>
                                  );
                                })}
                                {!g.sessions.length ? (
                                  <div className="px-2 py-2 text-xs text-zinc-500">
                                    No sessions
                                  </div>
                                ) : null}
                              </div>
                            ) : null}
                          </div>
                        );
                      })}

                      {canLoadMoreWorkspaces ? (
                        <div className="px-2 py-2">
                          <Button
                            variant="ghost"
                            className="w-full justify-center text-xs"
                            onClick={() =>
                              setWorkspacesVisible((v) => v + LIST_PAGE_SIZE)
                            }
                          >
                            Load more
                          </Button>
                        </div>
                      ) : null}

                      {!workspaceGroups.length ? (
                        <div className="px-2 py-6 text-center text-sm text-zinc-500">
                          No workspaces
                        </div>
                      ) : null}
                    </div>
                  </div>
                ) : null}

                <Separator />

                <div className="flex items-center justify-between gap-2 px-2">
                  <button
                    className="flex items-center gap-1 text-xs font-medium text-zinc-500 hover:text-zinc-700"
                    onClick={() => setSessionsPaneOpen((v) => !v)}
                  >
                    {sessionsPaneOpen ? (
                      <ChevronDown className="h-3.5 w-3.5" />
                    ) : (
                      <ChevronRight className="h-3.5 w-3.5" />
                    )}
                    Sessions
                  </button>
                </div>

                {sessionsPaneOpen ? (
                  <div className="min-h-0 flex-1 overflow-auto px-1">
                    <div className="space-y-1">
                      {visibleSessions.map((s) => {
                        const badge = statusBadge(s);
                        const active = s.id === selectedId;
                        const pinned = pinnedSessionIds.includes(s.id);
                        return (
                          <div
                            key={s.id}
                            className={[
                              "group flex items-start gap-1 rounded-md px-2 py-2",
                              active ? "bg-white shadow-sm" : "hover:bg-white/70",
                            ].join(" ")}
                          >
                            <button
                              onClick={() => selectSession(s.id)}
                              className="min-w-0 flex-1 text-left"
                            >
                              <div className="flex items-center justify-between gap-2">
                                <div className="min-w-0 flex items-center gap-1 text-sm text-zinc-900">
                                  {pinned ? (
                                    <Pin className="h-3.5 w-3.5 shrink-0 text-violet-600" />
                                  ) : null}
                                  <div className="truncate">{s.title || s.id}</div>
                                </div>
                                <Badge variant={badge.variant}>{badge.label}</Badge>
                              </div>
                              <div className="mt-1 truncate text-xs text-zinc-500">
                                {displayModelId(s.settings.model_id)}
                              </div>
                            </button>

                            <Button
                              variant="ghost"
                              size="icon"
                              className="h-8 w-8"
                              aria-label="Session actions"
                              onClick={(e) => {
                                e.preventDefault();
                                e.stopPropagation();
                                const rect = (
                                  e.currentTarget as HTMLButtonElement
                                ).getBoundingClientRect();
                                setSessionMenu((prev) => {
                                  if (prev?.sessionId === s.id) return null;
                                  return { sessionId: s.id, x: rect.right, y: rect.bottom };
                                });
                              }}
                            >
                              <MoreHorizontal className="h-4 w-4 text-zinc-500" />
                            </Button>
                          </div>
                        );
                      })}

                      {canLoadMoreSessions ? (
                        <div className="px-2 py-2">
                          <Button
                            variant="ghost"
                            className="w-full justify-center text-xs"
                            onClick={() =>
                              setSessionsVisible((v) => v + LIST_PAGE_SIZE)
                            }
                          >
                            Load more
                          </Button>
                        </div>
                      ) : null}
                      {!tmpSessions.length ? (
                        <div className="px-2 py-6 text-center text-sm text-zinc-500">
                          No sessions
                        </div>
                      ) : null}
                    </div>
                  </div>
                ) : null}
              </div>
            </div>

            <Separator />

            <div className="p-3">
              <Button
                variant="ghost"
                className="w-full justify-start gap-2"
                onClick={openSettings}
              >
                <Settings className="h-4 w-4 text-blue-600" />
                Settings
              </Button>
            </div>
          </aside>
        ) : null}

        <main className="flex min-w-0 flex-1 flex-col">
          <div className="flex items-start justify-between gap-4 border-b border-zinc-200 bg-white px-4 py-3">
            <div className="min-w-0 flex items-start gap-2">
              <Button
                variant="ghost"
                size="icon"
                className="h-8 w-8"
                aria-label={sidebarOpen ? "Hide sidebar" : "Show sidebar"}
                onClick={() => setSidebarOpen((v) => !v)}
              >
                {sidebarOpen ? (
                  <PanelLeftClose className="h-4 w-4 text-zinc-600" />
                ) : (
                  <PanelLeftOpen className="h-4 w-4 text-zinc-600" />
                )}
              </Button>
              <div className="min-w-0">
                <div className="truncate text-sm font-medium">
                  {selectedSummary?.title ?? "New thread"}
                </div>
                <div className="mt-0.5 flex items-center gap-2 text-xs text-zinc-600">
                  {selectedBadge ? (
                    <Badge variant={selectedBadge.variant}>{selectedBadge.label}</Badge>
                  ) : (
                    <Badge variant="idle">idle</Badge>
                  )}
                  {session ? (
                    <span className="truncate">
                      {session.settings.agent} · {displayModelId(session.settings.model_id)}
                    </span>
                  ) : null}
                </div>
              </div>
            </div>

            {session ? (
              <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">
                <label className="text-xs text-zinc-600">Agent</label>
                <select
                  className="h-8 rounded-md border border-zinc-200 bg-white px-2 text-xs"
                  value={session.settings.agent}
                  onChange={(e) => patchSession({ agent: e.target.value })}
                >
                  {agentOptions.map((a) => (
                    <option key={a} value={a}>
                      {a}
                    </option>
                  ))}
                </select>

                <label className="ml-2 text-xs text-zinc-600">Model</label>
                <select
                  className="h-8 min-w-[220px] rounded-md border border-zinc-200 bg-white px-2 text-xs"
                  value={session.settings.model_id}
                  onChange={(e) => patchSession({ model_id: e.target.value })}
                >
                  {modelOptions.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>

                <div className="flex max-w-[420px] items-center gap-2 rounded-md px-2 py-1">
                  <span className="text-xs text-zinc-600">workspace</span>
                  <span
                    className="min-w-0 truncate font-mono text-xs text-zinc-800"
                    title={session.settings.workspace_root ?? ""}
                  >
                    {workspaceBasename(session.settings.workspace_root ?? "")}
                  </span>
                </div>

                <Button
                  variant="ghost"
                  size="sm"
                  className="justify-start gap-2"
                  onClick={() => {
                    setExtraFolderPickerPath(session.settings.workspace_root ?? "");
                    setAddFolderOpen(true);
                  }}
                >
                  <FolderPlus className="h-4 w-4 text-violet-600" />
                  Add folder
                </Button>
              </div>
            ) : null}
          </div>

          <div
            ref={chatScrollRef}
            onScroll={updateIsAtBottom}
            className="flex-1 overflow-auto bg-zinc-50 px-4 py-4"
          >
            {selectedId ? (
              <div className="mx-auto w-full max-w-4xl space-y-3">
                {messages.map((m) => (
                  <MessageRow
                    key={`${m.role}:${m.id}`}
                    msg={m}
                    toolDurationsMs={toolDurationsMs}
                    thinkingDurationsMs={thinkingDurationsMs}
                    assistantDurationsMs={assistantDurationsMs}
                    onMermaidError={handleMermaidError}
                    onFork={forkSessionFromAssistant}
                  />
                ))}

                {stream.thinking ? (
                  <div className="flex justify-start">
                    <details className="relative w-full max-w-[78%] rounded-2xl border border-zinc-200 bg-zinc-50 px-4 py-2">
                      <button
                        type="button"
                        className="absolute right-2 top-2 rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
                        aria-label="Copy thinking"
                        title="Copy thinking"
                        onClick={() => copyToClipboard(stream.thinking)}
                      >
                        <Copy className="h-3.5 w-3.5" />
                      </button>
                      <summary className="cursor-pointer select-none pr-10 text-xs text-zinc-600">
                        thinking
                        {liveThinkingElapsedMs != null ? (
                          <span className="ml-2 text-zinc-500">
                            ({fmtDurationCompact(liveThinkingElapsedMs)})
                          </span>
                        ) : null}
                      </summary>
                      <div className="mt-2 whitespace-pre-wrap text-xs italic text-zinc-700">
                        {stream.thinking}
                      </div>
                    </details>
                  </div>
                ) : null}

                {stream.toolCalls.length ? (
                  <div className="space-y-2">
                    {stream.toolCalls.map((c) => (
                      <details
                        key={`toolcall:${c.id}:${c.name}`}
                        className="relative rounded-xl border border-zinc-200 bg-white px-4 py-2"
                      >
                        <button
                          type="button"
                          className="absolute right-2 top-2 rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
                          aria-label="Copy tool call"
                          title="Copy tool call"
                          onClick={() => copyToClipboard(c.arguments)}
                        >
                          <Copy className="h-3.5 w-3.5" />
                        </button>
                        <summary className="cursor-pointer select-none pr-10 text-xs text-zinc-700">
                          tool_call: <span className="font-mono">{c.name}</span>
                          {toolDurationsMs[c.id] != null ? (
                            <span className="ml-2 text-zinc-500">
                              ({fmtDurationCompact(toolDurationsMs[c.id]!)})
                            </span>
                          ) : toolStartsRef.current[c.id]?.startedAt != null ? (
                            <span className="ml-2 text-zinc-500">
                              ({fmtDurationCompact(clockNowMs - toolStartsRef.current[c.id]!.startedAt)})
                            </span>
                          ) : null}
                        </summary>
                        <CodeBlock className="mt-2" code={c.arguments} lang="json" copyButton={false} />
                      </details>
                    ))}
                  </div>
                ) : null}

                {stream.assistant ? (
                  <div className="flex justify-start">
                    <div
                      className={`${hasMermaidFence(stream.assistant) ? "w-full max-w-[92%]" : "max-w-[78%]"} rounded-2xl bg-zinc-50 px-4 py-2 text-sm text-zinc-900`}
                    >
                      <Markdown text={stream.assistant} deferMermaid />
                      <div className="mt-2 border-t border-zinc-200 pt-1">
                        <div className="flex items-center gap-1">
                          <button
                            type="button"
                            className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
                            aria-label="Copy message"
                            title="Copy message"
                            onClick={() => copyToClipboard(stream.assistant)}
                          >
                            <Copy className="h-4 w-4" />
                          </button>
                          <button
                            type="button"
                            disabled
                            className="cursor-not-allowed rounded-md p-1 text-zinc-500 opacity-40"
                            aria-label="Fork session"
                            title="Fork session from here"
                          >
                            <GitFork className="h-4 w-4" />
                          </button>
                          <button
                            type="button"
                            className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
                            aria-label="Menu"
                            title="Menu"
                          >
                            <MoreHorizontal className="h-4 w-4" />
                          </button>
                          {assistantStartedAtRef.current != null ? (
                            <span className="ml-auto text-xs text-zinc-500">
                              {fmtDurationCompact(liveAssistantElapsedMs ?? 0)}
                            </span>
                          ) : null}
                        </div>
                      </div>
                    </div>
                  </div>
                ) : null}

                <div ref={chatEndRef} />
              </div>
            ) : (
              <EmptyState />
            )}
          </div>

          {selectedId && !isAtBottom ? (
            <div className="fixed bottom-24 right-6 z-40">
              <Button
                variant="outline"
                size="icon"
                aria-label="Scroll to bottom"
                className="h-9 w-9 rounded-full shadow-sm"
                onClick={() => {
                  isAtBottomRef.current = true;
                  setIsAtBottom(true);
                  scrollToBottom("smooth");
                }}
              >
                <ArrowDown className="h-4 w-4 text-zinc-700" />
              </Button>
            </div>
          ) : null}

          <div className="border-t border-zinc-200 bg-white px-4 py-3">
            <div className="mx-auto w-full max-w-4xl">
              <div className="flex items-center gap-2">
                <div className="flex min-w-0 flex-1 items-center gap-2 rounded-3xl border border-zinc-200 bg-white px-4 py-2 shadow-sm hover:border-zinc-300 focus-within:border-blue-300 focus-within:ring-2 focus-within:ring-blue-500/20">
                  <Textarea
                    ref={composerRef}
                    value={composerText}
                    onChange={(e) => setComposerText(e.target.value)}
                    placeholder="Ask anything…"
                    className="min-h-[44px] max-h-[240px] min-w-0 flex-1 resize-none border-0 bg-transparent px-0 py-2 shadow-none focus-visible:ring-0 focus-visible:ring-offset-0"
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && !e.shiftKey) {
                        e.preventDefault();
                        onSend();
                      }
                    }}
                  />

                  {showInterrupt ? (
                    <Button
                      size="icon"
                      aria-label="Interrupt"
                      className="shrink-0 rounded-full bg-red-600 text-zinc-50 hover:bg-red-500"
                      onClick={async () => {
                        if (!cancellableRunId) return;
                        try {
                          await api.cancelRun(cancellableRunId);
                          await refreshSessions();
                        } catch (err) {
                          handleApiError(err);
                        }
                      }}
                    >
                      <Square className="h-4 w-4" />
                    </Button>
                  ) : (
                    <Button
                      size="icon"
                      aria-label="Send"
                      onClick={onSend}
                      disabled={!composerHasText}
                      className="shrink-0 rounded-full"
                    >
                      <ArrowUp className="h-4 w-4" />
                    </Button>
                  )}
                </div>

                <div className="flex items-center gap-1 rounded-3xl border border-zinc-200 bg-white px-2 py-2 shadow-sm">
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-9 w-9"
                    aria-label="Open workspace in VS Code"
                    title="Open workspace in VS Code"
                    disabled={!session}
                    onClick={() => openWorkspace("vscode")}
                  >
                    <Code className="h-4 w-4 text-blue-600" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-9 w-9"
                    aria-label="Open workspace in file manager"
                    title="Open workspace in file manager"
                    disabled={!session}
                    onClick={() => openWorkspace("file_manager")}
                  >
                    <FolderOpen className="h-4 w-4 text-violet-600" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-9 w-9"
                    aria-label="Open workspace in terminal"
                    title="Open workspace in terminal"
                    disabled={!session}
                    onClick={() => openWorkspace("terminal")}
                  >
                    <Terminal className="h-4 w-4 text-emerald-600" />
                  </Button>
                </div>
              </div>
            </div>
          </div>
        </main>
      </div>

      <Dialog open={settingsOpen} onOpenChange={setSettingsOpen}>
        <DialogContent className="max-w-3xl">
          <DialogHeader>
            <DialogTitle>kiliax.yaml</DialogTitle>
            <DialogDescription className="truncate">
              {configPath || "config"}
            </DialogDescription>
          </DialogHeader>
          <Textarea
            className="h-[420px] font-mono text-xs"
            value={configYaml}
            onChange={(e) => setConfigYaml(e.target.value)}
          />
          <div className="mt-3 flex justify-end gap-2">
            <Button variant="outline" onClick={() => setSettingsOpen(false)}>
              Cancel
            </Button>
            <Button onClick={saveConfig} disabled={savingConfig}>
              {savingConfig ? "Saving…" : "Save"}
            </Button>
          </div>
        </DialogContent>
      </Dialog>

      <Dialog open={skillsOpen} onOpenChange={setSkillsOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Skills</DialogTitle>
            <DialogDescription>Discovered from skills roots</DialogDescription>
          </DialogHeader>
          <div className="max-h-[420px] overflow-auto rounded-md border border-zinc-200">
            {skills.length ? (
              <div className="divide-y divide-zinc-200">
                {skills.map((s) => {
                  const enabled = skillsOverrides[s.id] ?? skillsDefaultEnable;
                  return (
                    <label
                      key={s.id}
                      className="flex items-center justify-between gap-3 bg-white px-3 py-2"
                    >
                      <div className="min-w-0">
                        <div className="truncate text-sm font-medium text-zinc-900">
                          {s.name}
                        </div>
                        <div className="mt-0.5 truncate text-xs text-zinc-600">
                          <span className="font-mono">{s.id}</span>
                          {s.description ? ` · ${s.description}` : ""}
                        </div>
                      </div>
                      <input
                        type="checkbox"
                        checked={enabled}
                        disabled={skillsSaving}
                        onChange={async (e) => {
                          const next = e.target.checked;
                          const prev = skillsOverrides[s.id];
                          setSkillsOverrides((o) => ({ ...o, [s.id]: next }));
                          setSkillsSaving(true);
                          try {
                            await api.patchConfigSkills({
                              skills: [{ id: s.id, enable: next }],
                            });
                          } catch (err) {
                            setSkillsOverrides((o) => {
                              const copy = { ...o };
                              if (prev === undefined) delete copy[s.id];
                              else copy[s.id] = prev;
                              return copy;
                            });
                            handleApiError(err);
                          } finally {
                            setSkillsSaving(false);
                          }
                        }}
                      />
                    </label>
                  );
                })}
              </div>
            ) : (
              <div className="bg-white px-3 py-6 text-center text-sm text-zinc-500">
                No skills
              </div>
            )}
          </div>
        </DialogContent>
      </Dialog>

      <Dialog open={mcpOpen} onOpenChange={setMcpOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>MCP</DialogTitle>
            <DialogDescription>Global MCP servers (kiliax.yaml)</DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            {(capabilities?.mcp_servers ?? []).map((s) => (
              <label
                key={s.id}
                className="flex items-center justify-between rounded-md border border-zinc-200 bg-white px-3 py-2"
              >
                <div className="min-w-0">
                  <div className="truncate text-sm">{s.id}</div>
                  <div className="mt-0.5 truncate text-xs text-zinc-600">
                    {s.state?.toString() ?? "unknown"}
                    {s.last_error ? ` · ${s.last_error}` : ""}
                  </div>
                </div>
                <input
                  type="checkbox"
                  checked={s.enable}
                  disabled={mcpSaving}
                  onChange={async (e) => {
                    const next = e.target.checked;
                    setMcpSaving(true);
                    try {
                      await api.patchConfigMcp({ servers: [{ id: s.id, enable: next }] });
                      await refreshCapabilities();
                    } catch (err) {
                      handleApiError(err);
                    } finally {
                      setMcpSaving(false);
                    }
                  }}
                />
              </label>
            ))}
            {!capabilities?.mcp_servers.length ? (
              <div className="text-center text-sm text-zinc-500">No MCP servers</div>
            ) : null}
          </div>
        </DialogContent>
      </Dialog>

      {sessionMenu ? (
        <div
          ref={sessionMenuRef}
          style={{ left: sessionMenu.x, top: sessionMenu.y }}
          className="fixed z-50 mt-1 w-44 -translate-x-full rounded-md border border-zinc-200 bg-white p-1 shadow-lg"
        >
          <button
            className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-zinc-800 hover:bg-zinc-100"
            onClick={() => {
              const id = sessionMenu.sessionId;
              togglePinnedSession(id);
              setSessionMenu(null);
            }}
          >
            <Pin className="h-4 w-4 text-violet-600" />
            {pinnedSessionIds.includes(sessionMenu.sessionId) ? "Unpin" : "Pin"}
          </button>
          <button
            className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-red-600 hover:bg-red-50"
            onClick={() => {
              setDeleteConfirm({ sessionId: sessionMenu.sessionId });
              setSessionMenu(null);
            }}
          >
            <Trash2 className="h-4 w-4" />
            Delete
          </button>
        </div>
      ) : null}

      {workspaceMenu ? (
        <div
          ref={workspaceMenuRef}
          style={{ left: workspaceMenu.x, top: workspaceMenu.y }}
          className="fixed z-50 mt-1 w-44 -translate-x-full rounded-md border border-zinc-200 bg-white p-1 shadow-lg"
        >
          <button
            className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-zinc-800 hover:bg-zinc-100"
            onClick={() => {
              const root = workspaceMenu.workspaceRoot;
              togglePinnedWorkspace(root);
              setWorkspaceMenu(null);
            }}
          >
            <Pin className="h-4 w-4 text-violet-600" />
            {pinnedWorkspaceRoots.includes(workspaceMenu.workspaceRoot) ? "Unpin" : "Pin"}
          </button>
          <button
            className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-red-600 hover:bg-red-50"
            onClick={() => {
              setWorkspaceDeleteConfirm({ workspaceRoot: workspaceMenu.workspaceRoot });
              setWorkspaceMenu(null);
            }}
          >
            <Trash2 className="h-4 w-4" />
            Delete
          </button>
        </div>
      ) : null}

      <Dialog open={Boolean(deleteConfirm)} onOpenChange={(open) => !open && setDeleteConfirm(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete session?</DialogTitle>
            <DialogDescription className="truncate">
              {deleteSessionSummary?.title ?? deleteSessionSummary?.id ?? ""}
            </DialogDescription>
          </DialogHeader>
          <div className="mt-3 flex justify-end gap-2">
            <Button variant="outline" onClick={() => setDeleteConfirm(null)}>
              Cancel
            </Button>
            <Button
              className="bg-red-600 text-zinc-50 hover:bg-red-500"
              onClick={async () => {
                const id = deleteConfirm?.sessionId;
                if (!id) return;
                setDeleteConfirm(null);
                await deleteSession(id);
              }}
            >
              Delete
            </Button>
          </div>
        </DialogContent>
      </Dialog>

      <Dialog
        open={Boolean(workspaceDeleteConfirm)}
        onOpenChange={(open) => !open && setWorkspaceDeleteConfirm(null)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete workspace?</DialogTitle>
            <DialogDescription className="truncate">
              {workspaceDeleteConfirm?.workspaceRoot ?? ""}
            </DialogDescription>
          </DialogHeader>
          <div className="mt-3 text-sm text-zinc-600">
            This deletes all sessions under this workspace (directory is not removed).
          </div>
          <div className="mt-3 flex justify-end gap-2">
            <Button variant="outline" onClick={() => setWorkspaceDeleteConfirm(null)}>
              Cancel
            </Button>
            <Button
              className="bg-red-600 text-zinc-50 hover:bg-red-500"
              onClick={async () => {
                const root = workspaceDeleteConfirm?.workspaceRoot;
                if (!root) return;
                setWorkspaceDeleteConfirm(null);
                await deleteWorkspace(root);
              }}
            >
              Delete
            </Button>
          </div>
        </DialogContent>
      </Dialog>

      <FolderPickerDialog
        open={workspaceCreateOpen}
        onOpenChange={setWorkspaceCreateOpen}
        title="Add workspace folder"
        description="Creates a new session in this workspace root."
        path={workspacePickerPath}
        onPathChange={setWorkspacePickerPath}
        confirmLabel="Create"
        confirmPending={workspaceCreateSaving}
        confirmPendingLabel="Creating…"
        confirmDisabled={!workspacePickerPath.trim() || workspaceCreateSaving}
        onConfirm={() => createSessionWithWorkspaceRoot(workspacePickerPath)}
      />

      <FolderPickerDialog
        open={addFolderOpen}
        onOpenChange={setAddFolderOpen}
        title="Add folder"
        description="Adds an extra directory this session can read/write."
        path={extraFolderPickerPath}
        onPathChange={setExtraFolderPickerPath}
        confirmLabel="Add"
        confirmPending={extraFolderSaving}
        confirmPendingLabel="Adding…"
        confirmDisabled={!extraFolderPickerPath.trim() || !session || extraFolderSaving}
        onConfirm={async () => {
          const path = extraFolderPickerPath.trim();
          if (!path) return;
          setExtraFolderSaving(true);
          try {
            await addExtraFolder(path);
            setAddFolderOpen(false);
          } finally {
            setExtraFolderSaving(false);
          }
        }}
      >
        {session?.settings.extra_workspace_roots?.length ? (
          <div className="rounded-md border border-zinc-200 bg-zinc-50 px-3 py-2 text-xs text-zinc-700">
            <div className="font-medium text-zinc-600">Existing</div>
            <div className="mt-1 space-y-1">
              {session.settings.extra_workspace_roots.map((p) => (
                <div key={p} className="flex items-center justify-between gap-2">
                  <div className="min-w-0 truncate font-mono text-xs" title={p}>
                    {p}
                  </div>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7"
                    aria-label="Remove folder"
                    title="Remove folder"
                    onClick={() => removeExtraFolder(p)}
                  >
                    <Trash2 className="h-4 w-4 text-zinc-500" />
                  </Button>
                </div>
              ))}
            </div>
          </div>
        ) : null}
      </FolderPickerDialog>
    </div>
  );
}
