import React, { useEffect, useMemo, useRef, useState } from "react";
import { ArrowDown, ArrowUp, MoreHorizontal, PanelLeftClose, PanelLeftOpen, Pin, Plus, Plug, Settings, Sparkles, Square, Trash2 } from "lucide-react";
import { api, ApiError, wsUrl } from "@/lib/api";
import { hrefToSession, navigate, useRoute } from "@/lib/router";
import type {
  Capabilities,
  Message,
  Session,
  SessionSummary,
  SkillSummary,
  ToolCall,
} from "@/lib/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { CodeBlock } from "@/components/code-block";
import { Markdown } from "@/components/markdown";
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
  createdAt: string;
  content: string;
};

type StreamState = {
  thinking: string;
  assistant: string;
  assistantStarted: boolean;
  toolCalls: Array<{ id: string; name: string; arguments: string }>;
};

type DebugError = {
  message: string;
  status?: number;
  code?: string;
  traceId?: string;
  details?: unknown;
  bodyText?: string;
};

const PINNED_SESSIONS_KEY = "kiliax:pinned_session_ids";
const SIDEBAR_OPEN_KEY = "kiliax:sidebar_open";

function displayModelId(modelId: string): string {
  const idx = modelId.indexOf("/");
  return idx === -1 ? modelId : modelId.slice(idx + 1);
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

function tmpWorkspacePath(): string {
  const now = new Date();
  const pad2 = (v: number) => v.toString().padStart(2, "0");
  const ts = [
    now.getUTCFullYear(),
    pad2(now.getUTCMonth() + 1),
    pad2(now.getUTCDate()),
    "T",
    pad2(now.getUTCHours()),
    pad2(now.getUTCMinutes()),
    pad2(now.getUTCSeconds()),
    "Z_",
    now.getUTCMilliseconds().toString().padStart(3, "0"),
  ].join("");
  return `~/.kiliax/workspace/tmp_${ts}`;
}

function renderToolCalls(toolCalls?: ToolCall[]) {
  if (!toolCalls?.length) return null;
  return (
    <div className="mt-2 space-y-1">
      {toolCalls.map((c) => (
        <details
          key={c.id}
          className="rounded-md border border-zinc-200 bg-white px-3 py-2"
        >
          <summary className="cursor-pointer select-none text-xs text-zinc-700">
            tool_call: <span className="font-mono">{c.name}</span>
          </summary>
          <CodeBlock className="mt-2" code={c.arguments} lang="json" />
        </details>
      ))}
    </div>
  );
}

function MessageRow({ msg }: { msg: Message }) {
  if (msg.role === "user") {
    return (
      <div className="flex justify-end">
        <div className="max-w-[78%] rounded-2xl bg-zinc-900 px-4 py-2 text-sm text-zinc-50">
          {msg.content}
        </div>
      </div>
    );
  }

  if (msg.role === "assistant") {
    return (
      <div className="flex justify-start">
        <div className="max-w-[78%] rounded-2xl bg-zinc-50 px-4 py-2 text-sm text-zinc-900">
          {msg.content ? (
            <Markdown text={msg.content} />
          ) : (
            <div className="text-zinc-500">…</div>
          )}
          {msg.reasoning_content ? (
            <details className="mt-2 rounded-md border border-zinc-200 bg-zinc-50 px-3 py-2">
              <summary className="cursor-pointer select-none text-xs text-zinc-600">
                thinking
              </summary>
              <div className="mt-2 whitespace-pre-wrap text-xs italic text-zinc-700">
                {msg.reasoning_content}
              </div>
            </details>
          ) : null}
          {renderToolCalls(msg.tool_calls ?? [])}
        </div>
      </div>
    );
  }

  return (
    <div className="flex justify-start">
      <details className="w-full max-w-[78%] rounded-2xl border border-zinc-200 bg-zinc-50 px-4 py-2">
        <summary className="cursor-pointer select-none text-xs text-zinc-700">
          tool_result: <span className="font-mono">{msg.tool_call_id}</span>
        </summary>
        <CodeBlock className="mt-2" code={msg.content} lang="json" />
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
  const [sidebarOpen, setSidebarOpen] = useState<boolean>(() => loadSidebarOpen());
  const [sessionsVisible, setSessionsVisible] = useState(6);
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
  const [isAtBottom, setIsAtBottom] = useState(true);

  const [composerText, setComposerText] = useState("");
  const [savingConfig, setSavingConfig] = useState(false);
  const [configYaml, setConfigYaml] = useState("");
  const [configPath, setConfigPath] = useState("");

  const [skillsOpen, setSkillsOpen] = useState(false);
  const [skills, setSkills] = useState<SkillSummary[]>([]);
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

  const [cwdOpen, setCwdOpen] = useState(false);
  const [cwdDraft, setCwdDraft] = useState("");

  const [authError, setAuthError] = useState<string | null>(null);
  const [debugError, setDebugError] = useState<DebugError | null>(null);

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
  const visibleSessions = useMemo(
    () => sortedSessions.slice(0, sessionsVisible),
    [sortedSessions, sessionsVisible],
  );
  const deleteSessionSummary = deleteConfirm
    ? sortedSessions.find((s) => s.id === deleteConfirm.sessionId) ?? null
    : null;

  function handleApiError(err: unknown) {
    if (err instanceof ApiError && err.status === 401) {
      setAuthError("Unauthorized. Re-open the URL printed by `kiliax serve start`.");
      setDebugError(null);
      return;
    }
    if (err instanceof ApiError) {
      setDebugError({
        message: err.message,
        status: err.status,
        code: err.code,
        traceId: err.traceId,
        details: err.details,
        bodyText: err.bodyText,
      });
    } else {
      const message = err instanceof Error ? err.message : String(err);
      setDebugError({ message });
    }
    // eslint-disable-next-line no-console
    console.error(err);
  }

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

    if (type === "assistant_thinking_delta") {
      const delta = ev?.data?.delta ?? "";
      if (!delta) return;
      setStream((s) => {
        if (s.assistantStarted) return s;
        return { ...s, thinking: s.thinking + delta };
      });
      return;
    }

    if (type === "assistant_delta") {
      const delta = ev?.data?.delta ?? "";
      if (!delta) return;
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
      setStream((s) => ({
        ...s,
        toolCalls: [
          ...s.toolCalls,
          { id: String(call.id ?? ""), name: String(call.name), arguments: String(call.arguments ?? "") },
        ],
      }));
      return;
    }

    if (type === "tool_result") {
      const msg = ev?.data?.message;
      if (!msg?.role) return;
      setMessages((m) => [...m, msg as Message]);
      return;
    }

    if (type === "assistant_message") {
      const msg = ev?.data?.message;
      if (!msg?.role) return;
      setMessages((m) => [...m, msg as Message]);
      setStream({ thinking: "", assistant: "", assistantStarted: false, toolCalls: [] });
      return;
    }

    if (type === "session_settings_changed") {
      if (selectedId) {
        await fetchSession(selectedId);
      }
      return;
    }

    if (type === "run_done" || type === "run_error" || type === "run_cancelled") {
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

  async function onSend() {
    const text = composerText.trim();
    if (!text) return;

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

      const run: any = await api.createRun(sessionId, {
        input: { type: "text", text },
      });
      setPending((p) => [
        ...p,
        {
          sessionId,
          runId: String(run?.id ?? ""),
          createdAt: new Date().toISOString(),
          content: text,
        },
      ]);
      setComposerText("");
      await refreshSessions();
    } catch (err) {
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

  async function openSkills() {
    try {
      const res = await api.listGlobalSkills();
      setSkills(res.items);
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
    saveSidebarOpen(sidebarOpen);
  }, [sidebarOpen]);

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
    setSessionsVisible((v) => {
      const min = 6;
      const next = Math.max(min, v);
      return Math.min(next, sortedSessions.length || next);
    });
  }, [sortedSessions.length]);

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
  const cancellableRunId =
    selectedSummary?.status.active_run_id ??
    pendingForSelected[pendingForSelected.length - 1]?.runId ??
    null;
  const showInterrupt = Boolean(selectedId) && Boolean(cancellableRunId) && !composerHasText;

  const agentOptions = capabilities?.agents ?? [];
  const modelOptions = capabilities?.models ?? [];

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
      <Dialog
        open={Boolean(debugError)}
        onOpenChange={(open) => {
          if (!open) setDebugError(null);
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>Request failed</DialogTitle>
            <DialogDescription>
              {debugError?.status ? (
                <span>
                  HTTP {debugError.status}
                  {debugError.code ? ` • ${debugError.code}` : ""}
                </span>
              ) : (
                <span>Unexpected error</span>
              )}
            </DialogDescription>
          </DialogHeader>
          <pre className="max-h-[30vh] overflow-auto rounded-md bg-zinc-50 p-3 text-xs text-zinc-800">
            {debugError?.message ?? ""}
          </pre>
          {debugError?.traceId ? (
            <div className="text-xs text-zinc-600">
              trace_id: <span className="font-mono">{debugError.traceId}</span>
            </div>
          ) : null}
          {debugError?.details != null ? (
            <details className="rounded-md border border-zinc-200 bg-white px-3 py-2">
              <summary className="cursor-pointer select-none text-xs text-zinc-700">
                details
              </summary>
              <pre className="mt-2 max-h-[30vh] overflow-auto rounded bg-zinc-50 p-2 text-xs text-zinc-800">
                {stringifyUnknown(debugError.details)}
              </pre>
            </details>
          ) : null}
          {debugError?.bodyText && !debugError?.details ? (
            <details className="rounded-md border border-zinc-200 bg-white px-3 py-2">
              <summary className="cursor-pointer select-none text-xs text-zinc-700">
                raw body
              </summary>
              <pre className="mt-2 max-h-[30vh] overflow-auto rounded bg-zinc-50 p-2 text-xs text-zinc-800">
                {debugError.bodyText}
              </pre>
            </details>
          ) : null}
        </DialogContent>
      </Dialog>
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

            <div className="flex-1 overflow-auto p-2">
              <div className="px-2 pb-2 text-xs font-medium text-zinc-500">
                Sessions
              </div>
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
                          const rect = (e.currentTarget as HTMLButtonElement).getBoundingClientRect();
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
                {!sortedSessions.length ? (
                  <div className="px-2 py-6 text-center text-sm text-zinc-500">
                    No sessions
                  </div>
                ) : null}
                {sortedSessions.length > sessionsVisible ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="w-full justify-start text-xs text-zinc-600"
                    onClick={() => setSessionsVisible((v) => v + 6)}
                  >
                    Load more
                  </Button>
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

                <Button
                  variant="ghost"
                  size="sm"
                  className="max-w-[360px] justify-start gap-2"
                  onClick={() => {
                    setCwdDraft(session.settings.workspace_root ?? "");
                    setCwdOpen(true);
                  }}
                >
                  <span className="text-xs text-zinc-600">cwd</span>
                  <span className="min-w-0 truncate font-mono text-xs text-zinc-800">
                    {session.settings.workspace_root ?? ""}
                  </span>
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
                  <MessageRow key={`${m.role}:${m.id}`} msg={m} />
                ))}

                {pendingForSelected.map((p) => (
                  <div key={`pending:${p.runId}`} className="flex justify-end">
                    <div className="max-w-[78%] rounded-2xl bg-zinc-900/90 px-4 py-2 text-sm text-zinc-50">
                      {p.content}
                    </div>
                  </div>
                ))}

                {stream.thinking ? (
                  <div className="flex justify-start">
                    <details className="w-full max-w-[78%] rounded-2xl border border-zinc-200 bg-zinc-50 px-4 py-2">
                      <summary className="cursor-pointer select-none text-xs text-zinc-600">
                        thinking
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
                        className="rounded-xl border border-zinc-200 bg-white px-4 py-2"
                      >
                        <summary className="cursor-pointer select-none text-xs text-zinc-700">
                          tool_call: <span className="font-mono">{c.name}</span>
                        </summary>
                        <CodeBlock className="mt-2" code={c.arguments} lang="json" />
                      </details>
                    ))}
                  </div>
                ) : null}

                {stream.assistant ? (
                  <div className="flex justify-start">
                    <div className="max-w-[78%] rounded-2xl bg-zinc-50 px-4 py-2 text-sm text-zinc-900">
                      <Markdown text={stream.assistant} />
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
              <div className="flex items-center gap-2 rounded-3xl border border-zinc-200 bg-white px-4 py-2 shadow-sm hover:border-zinc-300 focus-within:border-blue-300 focus-within:ring-2 focus-within:ring-blue-500/20">
                <Textarea
                  ref={composerRef}
                  value={composerText}
                  onChange={(e) => setComposerText(e.target.value)}
                  placeholder="畅所欲问"
                  className="min-h-[44px] max-h-[240px] resize-none border-0 bg-transparent px-0 py-2 shadow-none focus-visible:ring-0 focus-visible:ring-offset-0"
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
                    className="shrink-0 rounded-full"
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
          <div className="max-h-[360px] overflow-auto rounded-md border border-zinc-200">
            {skills.length ? (
              <div className="divide-y divide-zinc-200">
                {skills.map((s) => (
                  <div key={s.id} className="px-3 py-2">
                    <div className="text-sm font-medium">{s.name}</div>
                    <div className="mt-0.5 text-xs text-zinc-600">
                      <span className="font-mono">{s.id}</span>
                      {s.description ? ` · ${s.description}` : ""}
                    </div>
                  </div>
                ))}
              </div>
            ) : (
              <div className="px-3 py-6 text-center text-sm text-zinc-500">
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

      <Dialog open={cwdOpen} onOpenChange={setCwdOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Workspace (cwd)</DialogTitle>
            <DialogDescription>
              Must be under <span className="font-mono">~/.kiliax</span>.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <Input
              value={cwdDraft}
              onChange={(e) => setCwdDraft(e.target.value)}
              placeholder="~/.kiliax/workspace/tmp_..."
            />
            <div className="flex justify-between gap-2">
              <Button
                variant="outline"
                onClick={() => setCwdDraft(tmpWorkspacePath())}
              >
                New tmp
              </Button>
              <div className="flex gap-2">
                <Button variant="outline" onClick={() => setCwdOpen(false)}>
                  Cancel
                </Button>
                <Button
                  onClick={() => {
                    patchSession({ workspace_root: cwdDraft });
                    setCwdOpen(false);
                  }}
                  disabled={!cwdDraft.trim()}
                >
                  Apply
                </Button>
              </div>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}
