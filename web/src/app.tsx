import React, { useEffect, useMemo, useRef, useState } from "react";
import { ArrowDown, ArrowUp, ChevronDown, ChevronRight, Code, Copy, FolderOpen, FolderPlus, GitFork, MoreHorizontal, PanelLeftClose, PanelLeftOpen, Pencil, Pin, Plus, Plug, RefreshCcw, Settings, Sparkles, Square, Star, Terminal, Trash2, X } from "lucide-react";
import { api, ApiError } from "./lib/api";
import { hrefToSession, navigate, useRoute } from "./lib/router";
import { copyToClipboard, fmtDurationCompact, hasMermaidFence, messageIdToSafeNumber, modelLabel, monotonicNowMs, newAlertId, parseMessageId, splitModelId, useOverlaySidebarViewport } from "./lib/app-utils";
import { loadPinnedSessionIds, loadPinnedWorkspaceRoots, loadSidebarOpen, loadSessionsPaneOpen, loadWorkspacesPaneOpen, savePinnedSessionIds, savePinnedWorkspaceRoots, saveSidebarOpen, saveSessionsPaneOpen, saveWorkspacesPaneOpen } from "./lib/preferences";
import { statusBadge, sortSessions } from "./lib/session-utils";
import { useWsEvents } from "./lib/use-ws-events";
import { cn } from "./lib/utils";
import { isTmpWorkspaceRoot, workspaceBasename, workspaceDisplayName } from "./lib/workspace-utils";
import type {
  Capabilities,
  ConfigProviderSummary,
  ConfigProvidersResponse,
  ConfigRuntimeResponse,
  Message,
  Session,
  SessionSummary,
  SkillLoadError,
  SkillSummary,
} from "./lib/types";
import { AlertStack, type AlertItem } from "./components/alert-stack";
import { ActionSheet } from "./components/ui/action-sheet";
import { Badge } from "./components/ui/badge";
import { Button } from "./components/ui/button";
import { CodeBlock } from "./components/code-block";
import { EmptyState } from "./components/empty-state";
import { FolderPickerDialog } from "./components/folder-picker";
import { Markdown, type MermaidErrorInfo } from "./components/markdown";
import { MessageRow } from "./components/message-row";
import { SessionItemRow } from "./components/session-item-row";
import { Input } from "./components/ui/input";
import { Separator } from "./components/ui/separator";
import { Sheet, SheetClose, SheetContent } from "./components/ui/sheet";
import { Textarea } from "./components/ui/textarea";

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

type ProviderDraft = {
  id: string;
  baseUrl: string;
  models: string[];
  apiKeySet: boolean;
  apiKeyDraft: string;
  modelDraft: string;
};

const LIST_PAGE_SIZE = 6;
const PROVIDERS_PANE_DEFAULT_MODEL = "__default_model__";
const PROVIDERS_PANE_NEW_PROVIDER = "__new_provider__";

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
    loadWorkspacesPaneOpen(true),
  );
  const [sessionsPaneOpen, setSessionsPaneOpen] = useState<boolean>(() =>
    loadSessionsPaneOpen(true),
  );
  const [sessionsVisible, setSessionsVisible] = useState<number>(LIST_PAGE_SIZE);
  const [workspacesVisible, setWorkspacesVisible] = useState<number>(LIST_PAGE_SIZE);
  const [expandedWorkspaces, setExpandedWorkspaces] = useState<Record<string, boolean>>({});
  const route = useRoute();
  const isNarrowViewport = useOverlaySidebarViewport();
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
  const sessionsRefreshAtRef = useRef(0);
  const sessionsRefreshInFlightRef = useRef<Promise<void> | null>(null);
  const toolStartsRef = useRef<Record<string, { name: string; startedAt: number }>>({});
  const thinkingStartedAtRef = useRef<number | null>(null);
  const assistantStartedAtRef = useRef<number | null>(null);
  const nextThinkingDurationMsRef = useRef<number | null>(null);
  const [isAtBottom, setIsAtBottom] = useState(true);

  const [composerText, setComposerText] = useState("");
  const [editMessageOpen, setEditMessageOpen] = useState(false);
  const [editMessageId, setEditMessageId] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [editSaving, setEditSaving] = useState(false);
  const [savingConfig, setSavingConfig] = useState(false);
  const [configYaml, setConfigYaml] = useState("");
  const [configPath, setConfigPath] = useState("");
  const [configLoaded, setConfigLoaded] = useState(false);

  const [skillsOpen, setSkillsOpen] = useState(false);
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [skillsDefaultEnable, setSkillsDefaultEnable] = useState(true);
  const [skillsOverrides, setSkillsOverrides] = useState<Record<string, boolean>>({});
  const [skillsLoadErrors, setSkillsLoadErrors] = useState<SkillLoadError[]>([]);
  const [skillsSaving, setSkillsSaving] = useState(false);
  const [skillsDefaultsSaving, setSkillsDefaultsSaving] = useState(false);
  const [mcpOpen, setMcpOpen] = useState(false);
  const [mcpSaving, setMcpSaving] = useState(false);
  const [mcpDefaultsSaving, setMcpDefaultsSaving] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<"providers" | "agents" | "yaml">("providers");
  const [settingsLoading, setSettingsLoading] = useState(false);
  const [settingsSaving, setSettingsSaving] = useState(false);
  const [modelDefaultsSaving, setModelDefaultsSaving] = useState(false);

  const [settingsProvidersDefaultModel, setSettingsProvidersDefaultModel] = useState("");
  const [settingsProviders, setSettingsProviders] = useState<ProviderDraft[]>([]);
  const [providersPaneSelection, setProvidersPaneSelection] = useState<string>("");
  const [newProviderId, setNewProviderId] = useState("");
  const [newProviderBaseUrl, setNewProviderBaseUrl] = useState("");
  const [newProviderModels, setNewProviderModels] = useState<string[]>([]);
  const [newProviderModelDraft, setNewProviderModelDraft] = useState("");
  const [newProviderApiKey, setNewProviderApiKey] = useState("");

  const [runtimeMaxSteps, setRuntimeMaxSteps] = useState("");
  const [agentsPlanMaxSteps, setAgentsPlanMaxSteps] = useState("");
  const [agentsGeneralMaxSteps, setAgentsGeneralMaxSteps] = useState("");

  const [sessionMenu, setSessionMenu] = useState<{
    sessionId: string;
    x: number;
    y: number;
  } | null>(null);
  const [sessionActionSheet, setSessionActionSheet] = useState<{
    sessionId: string;
  } | null>(null);
  const sessionMenuRef = useRef<HTMLDivElement | null>(null);

  const [deleteConfirm, setDeleteConfirm] = useState<{
    sessionId: string;
  } | null>(null);

  const [workspaceDeleteConfirm, setWorkspaceDeleteConfirm] = useState<{
    workspaceRoot: string;
  } | null>(null);

  const [providerDeleteConfirm, setProviderDeleteConfirm] = useState<{
    providerId: string;
  } | null>(null);

  const [workspaceMenu, setWorkspaceMenu] = useState<{
    workspaceRoot: string;
    x: number;
    y: number;
  } | null>(null);
  const [workspaceActionSheet, setWorkspaceActionSheet] = useState<{
    workspaceRoot: string;
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

  const lastEventIdRef = useRef(0);
  const chatScrollRef = useRef<HTMLDivElement | null>(null);
  const chatEndRef = useRef<HTMLDivElement | null>(null);
  const composerRef = useRef<HTMLTextAreaElement | null>(null);
  const selectedIdRef = useRef<string | null>(selectedId);
  const skipNextFetchRef = useRef<string | null>(null);
  const isAtBottomRef = useRef(true);
  const wsEvents = useWsEvents({
    onEvent: handleEvent,
    isSessionCurrent: (id) => selectedIdRef.current === id,
    getAfterEventId: () => lastEventIdRef.current,
  });

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

  const providersPaneSelectedProvider = useMemo(() => {
    return settingsProviders.find((p) => p.id === providersPaneSelection) ?? null;
  }, [settingsProviders, providersPaneSelection]);

  const defaultModelSuggestions = useMemo(() => {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const p of settingsProviders) {
      for (const m of p.models ?? []) {
        const trimmed = (m ?? "").trim();
        if (!trimmed) continue;
        const qualified = trimmed.includes("/") ? trimmed : `${p.id}/${trimmed}`;
        if (seen.has(qualified)) continue;
        seen.add(qualified);
        out.push(qualified);
      }
    }
    out.sort();
    return out;
  }, [settingsProviders]);

  function handleApiError(err: unknown) {
    if (err instanceof ApiError && err.status === 401) {
      setAuthError("Unauthorized. Re-open the URL printed by `kiliax server start`.");
      setAlerts([]);
      return;
    }
    if (err instanceof ApiError) {
      const subtitle = err.code ? `HTTP ${err.status} • ${err.code}` : `HTTP ${err.status}`;
      const details =
        err.details != null ? err.details : err.bodyText ? { raw_body: err.bodyText } : undefined;
      pushAlert({
        id: newAlertId("api"),
        level: "error",
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
        level: "error",
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

  async function copyWithToast(value: string, label: string) {
    const text = (value ?? "").trim();
    if (!text) return;
    const ok = await copyToClipboard(text);
    if (ok) {
      pushAlert({
        id: newAlertId("copy"),
        level: "success",
        title: "Copied",
        message: `${label} copied to clipboard.`,
        autoCloseMs: 1600,
      });
      return;
    }
    pushAlert({
      id: newAlertId("copy"),
      level: "error",
      title: "Copy failed",
      message: "Clipboard access was blocked by the browser.",
      autoCloseMs: 4000,
    });
  }

  function handleMermaidError(info: MermaidErrorInfo) {
    const key = (info.key ?? "").trim();
    if (key && seenMermaidAlertKeysRef.current.has(key)) return;
    if (key) seenMermaidAlertKeysRef.current.add(key);

    pushAlert({
      id: newAlertId("mermaid"),
      level: "error",
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
      sessionsRefreshAtRef.current = monotonicNowMs();
      setAuthError(null);
    } catch (err) {
      handleApiError(err);
    }
  }

  async function refreshSessionsIfStale(minIntervalMs = 1500) {
    const now = monotonicNowMs();
    if (now - sessionsRefreshAtRef.current < minIntervalMs) return;
    if (sessionsRefreshInFlightRef.current) {
      await sessionsRefreshInFlightRef.current;
      return;
    }
    const promise = refreshSessions().finally(() => {
      sessionsRefreshInFlightRef.current = null;
    });
    sessionsRefreshInFlightRef.current = promise;
    await promise;
  }

  function selectSession(sessionId: string) {
    navigate(hrefToSession(sessionId));
    if (isNarrowViewport) setSidebarOpen(false);
  }

  function openSessionMenuAt(sessionId: string, anchor: DOMRect) {
    if (isNarrowViewport) {
      setSessionActionSheet((prev) => {
        if (prev?.sessionId === sessionId) return null;
        return { sessionId };
      });
      return;
    }
    setSessionMenu((prev) => {
      if (prev?.sessionId === sessionId) return null;
      return { sessionId, x: anchor.right, y: anchor.bottom };
    });
  }

  function openWorkspaceMenuAt(workspaceRoot: string, anchor: DOMRect) {
    if (isNarrowViewport) {
      setWorkspaceActionSheet((prev) => {
        if (prev?.workspaceRoot === workspaceRoot) return null;
        return { workspaceRoot };
      });
      return;
    }
    setWorkspaceMenu((prev) => {
      if (prev?.workspaceRoot === workspaceRoot) return null;
      return { workspaceRoot, x: anchor.right, y: anchor.bottom };
    });
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

      lastEventIdRef.current = s.status.last_event_id ?? 0;
      wsEvents.resetReconnectAttempt();
      wsEvents.connect(sessionId, lastEventIdRef.current);
    } catch (err) {
      handleApiError(err);
    }
  }

  async function handleEvent(ev: any) {
    const type = ev?.type ?? ev?.event_type;
    if (!type) return;
    const eventId = ev?.event_id;
    if (typeof eventId === "number" && Number.isFinite(eventId)) {
      lastEventIdRef.current = Math.max(lastEventIdRef.current, eventId);
    }
    const now = monotonicNowMs();

    if (type === "events_lagged") {
      const sid = selectedIdRef.current;
      if (sid) {
        await fetchSession(sid);
      }
      return;
    }

    if (type === "step_start") {
      thinkingStartedAtRef.current = now;
      assistantStartedAtRef.current = null;
      nextThinkingDurationMsRef.current = null;
      void refreshSessionsIfStale();
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
      void refreshSessionsIfStale();
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
      void refreshSessionsIfStale();
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
      void refreshSessionsIfStale();
      return;
    }

    if (type === "session_settings_changed") {
      if (selectedId) {
        await fetchSession(selectedId);
      }
      await refreshSessionsIfStale(0);
      return;
    }

    if (type === "session_messages_reset") {
      if (selectedId) {
        await fetchSession(selectedId);
      }
      await refreshSessionsIfStale(0);
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
        level: "error",
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
      await refreshSessionsIfStale(0);
      return;
    }
  }

  function inheritedModelIdForNewSession(): string | undefined {
    const v = (
      session?.settings?.model_id ??
      selectedSummary?.settings?.model_id ??
      ""
    ).trim();
    return v ? v : undefined;
  }

  async function onNewSession() {
    try {
      const modelId = inheritedModelIdForNewSession();
      const s = await api.createSession(modelId ? { settings: { model_id: modelId } } : undefined);
      await refreshSessions();
      selectSession(s.id);
    } catch (err) {
      handleApiError(err);
    }
  }

  async function onNewSessionInWorkspace(workspaceRoot: string) {
    try {
      const modelId = inheritedModelIdForNewSession();
      const s = await api.createSession({
        settings: { workspace_root: workspaceRoot, model_id: modelId },
      });
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
    const deleteWorkspaceRoot = isTmpWorkspaceRoot(workspaceRoot);
    try {
      for (const id of ids) {
        await api.deleteSession(id, { delete_workspace_root: deleteWorkspaceRoot });
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
      const modelId = inheritedModelIdForNewSession();
      const s = await api.createSession({
        settings: { workspace_root: trimmed, model_id: modelId },
      });
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

  function resetStreamState() {
    setStream({ thinking: "", assistant: "", assistantStarted: false, toolCalls: [] });
    setToolDurationsMs({});
    setAssistantDurationsMs({});
    setThinkingDurationsMs({});
    toolStartsRef.current = {};
    thinkingStartedAtRef.current = null;
    assistantStartedAtRef.current = null;
    nextThinkingDurationMsRef.current = null;
  }

  function openEditDialog(userMessageId: string, content: string) {
    if (!selectedId) return;
    if (parseMessageId(userMessageId) == null) return;
    setEditMessageId(userMessageId);
    setEditDraft(content);
    setEditMessageOpen(true);
  }

  async function saveEditAndSend() {
    if (!selectedId || editMessageId == null) return;
    const text = editDraft.trim();
    if (!text) return;

    const sessionId = selectedId;
    const baseEventId = selectedSummary?.status.last_event_id ?? 0;
    setEditSaving(true);
    try {
      const afterId = parseMessageId(editMessageId);
      if (afterId == null) throw new Error("Invalid user message id");
      const afterIdNum = messageIdToSafeNumber(afterId);
      setMessages((prev) =>
        prev
          .map((m) => {
            if (m.role === "user" && m.id === editMessageId) {
              return { ...m, content: text };
            }
            return m;
          })
          .filter((m) => {
            const mid = parseMessageId(m.id);
            if (mid == null) return false;
            return mid <= afterId;
          }),
      );
      resetStreamState();
      setPending((p) => p.filter((m) => m.sessionId !== sessionId));

      const run: any = await api.createRun(sessionId, {
        input: { type: "edit_user_message", user_message_id: afterIdNum, content: text },
      });
      const runId = String(run?.id ?? "").trim();
      setPending((p) => [
        ...p,
        {
          sessionId,
          runId,
          baseEventId,
          createdAt: new Date().toISOString(),
          content: text,
        },
      ]);
      setEditMessageOpen(false);
      await refreshSessions();
    } catch (err) {
      handleApiError(err);
      if (selectedIdRef.current === sessionId) {
        await fetchSession(sessionId);
      }
    } finally {
      setEditSaving(false);
    }
  }

  async function regenerateAssistant(assistantMessageId: string) {
    if (!selectedId) return;
    const sessionId = selectedId;
    const baseEventId = selectedSummary?.status.last_event_id ?? 0;

    const assistantIdx = messages.findIndex(
      (m) => m.role === "assistant" && m.id === assistantMessageId,
    );
    const precedingUser =
      assistantIdx === -1
        ? null
        : (() => {
            for (let i = assistantIdx - 1; i >= 0; i -= 1) {
              const m = messages[i];
              if (m?.role === "user") return m;
            }
            return null;
          })();
    const afterId = precedingUser ? parseMessageId(precedingUser.id) : null;

    try {
      if (afterId == null) {
        throw new Error("Could not locate preceding user message");
      }
      if (afterId != null) {
        setMessages((prev) =>
          prev.filter((m) => {
            const mid = parseMessageId(m.id);
            if (mid == null) return false;
            return mid <= afterId;
          }),
        );
      }
      resetStreamState();
      setPending((p) => p.filter((m) => m.sessionId !== sessionId));

      const userIdNum = messageIdToSafeNumber(afterId);
      const run: any = await api.createRun(sessionId, {
        input: {
          type: "regenerate_after_user_message",
          user_message_id: userIdNum,
        },
      });
      const runId = String(run?.id ?? "").trim();
      setPending((p) => [
        ...p,
        {
          sessionId,
          runId,
          baseEventId,
          createdAt: new Date().toISOString(),
          content: precedingUser?.content ?? "",
        },
      ]);
      await refreshSessions();
    } catch (err) {
      handleApiError(err);
      if (selectedIdRef.current === sessionId) {
        await fetchSession(sessionId);
      }
    }
  }

  async function deleteSession(sessionId: string) {
    try {
      const root =
        sessions.find((s) => s.id === sessionId)?.settings.workspace_root ?? "";
      await api.deleteSession(sessionId, {
        delete_workspace_root: isTmpWorkspaceRoot(root),
      });
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
    if (isNarrowViewport) setSidebarOpen(false);
    try {
      const skillsRes = selectedId
        ? await api.listSkills(selectedId)
        : await api.listGlobalSkills();
      setSkills(skillsRes.items);
      setSkillsLoadErrors(skillsRes.errors ?? []);

      const activeSkillsSettings = selectedId
        ? (session?.settings.skills ?? selectedSummary?.settings.skills)
        : null;
      setSkillsDefaultEnable(activeSkillsSettings?.default_enable ?? true);
      const nextOverrides: Record<string, boolean> = {};
      for (const s of activeSkillsSettings?.overrides ?? []) {
        if (!s?.id) continue;
        nextOverrides[s.id] = Boolean(s.enable);
      }
      setSkillsOverrides(nextOverrides);
      setSkillsOpen(true);
    } catch (err) {
      handleApiError(err);
    }
  }

  function normalizeModels(models: string[]): string[] {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const raw of models ?? []) {
      const v = (raw ?? "").trim();
      if (!v) continue;
      if (seen.has(v)) continue;
      seen.add(v);
      out.push(v);
    }
    return out;
  }

  function parseModelTokens(raw: string): string[] {
    if (!raw) return [];
    return normalizeModels(raw.split(/[\r\n,]+/g));
  }

  function normalizeProviderDraft(p: ConfigProviderSummary): ProviderDraft {
    return {
      id: p.id,
      baseUrl: p.base_url ?? "",
      models: normalizeModels(p.models ?? []),
      apiKeySet: Boolean(p.api_key_set),
      apiKeyDraft: "",
      modelDraft: "",
    };
  }

  async function loadSettingsProviders() {
    const res: ConfigProvidersResponse = await api.getConfigProviders();
    setSettingsProvidersDefaultModel(res.default_model ?? "");
    const providers = (res.providers ?? []).map(normalizeProviderDraft);
    providers.sort((a, b) => a.id.localeCompare(b.id));
    setSettingsProviders(providers);
    setProvidersPaneSelection((prev) => {
      const cur = (prev ?? "").trim();
      if (cur === PROVIDERS_PANE_DEFAULT_MODEL || cur === PROVIDERS_PANE_NEW_PROVIDER) {
        return cur;
      }
      if (cur && providers.some((p) => p.id === cur)) return cur;
      if (providers.length) return providers[0].id;
      return PROVIDERS_PANE_NEW_PROVIDER;
    });
  }

  async function loadSettingsRuntime() {
    const res: ConfigRuntimeResponse = await api.getConfigRuntime();
    setRuntimeMaxSteps(res.runtime_max_steps != null ? String(res.runtime_max_steps) : "");
    setAgentsPlanMaxSteps(
      res.agents_plan_max_steps != null ? String(res.agents_plan_max_steps) : "",
    );
    setAgentsGeneralMaxSteps(
      res.agents_general_max_steps != null ? String(res.agents_general_max_steps) : "",
    );
  }

  async function refreshAfterConfigChange() {
    await refreshCapabilities();
    await refreshSessions();
    if (selectedId) await fetchSession(selectedId);
  }

  async function openSettings() {
    if (isNarrowViewport) setSidebarOpen(false);
    setSettingsLoading(true);
    try {
      setProvidersPaneSelection("");
      await Promise.all([loadSettingsProviders(), loadSettingsRuntime()]);
      setSettingsTab("providers");
      setConfigYaml("");
      setConfigPath("");
      setConfigLoaded(false);
      setNewProviderId("");
      setNewProviderBaseUrl("");
      setNewProviderModels([]);
      setNewProviderModelDraft("");
      setNewProviderApiKey("");
      setSettingsOpen(true);
    } catch (err) {
      handleApiError(err);
    } finally {
      setSettingsLoading(false);
    }
  }

  async function ensureConfigLoaded(force = false) {
    if (!force && configLoaded) return;
    const cfg = await api.getConfig();
    setConfigYaml(cfg.yaml);
    setConfigPath(cfg.path);
    setConfigLoaded(true);
  }

  async function openMcp() {
    if (isNarrowViewport) setSidebarOpen(false);
    if (selectedId) {
      await fetchSession(selectedId);
    } else {
      await refreshCapabilities();
    }
    setMcpOpen(true);
  }

  async function saveConfig() {
    setSavingConfig(true);
    try {
      await api.putConfig({ yaml: configYaml });
      pushAlert({
        id: newAlertId("config"),
        level: "success",
        title: "Saved",
        message: "Config updated.",
        autoCloseMs: 2500,
      });
      await Promise.all([loadSettingsProviders(), loadSettingsRuntime()]);
      await refreshAfterConfigChange();
    } catch (err) {
      handleApiError(err);
    } finally {
      setSavingConfig(false);
    }
  }

  function setProviderDraft(id: string, patch: Partial<ProviderDraft>) {
    setSettingsProviders((prev) =>
      prev.map((p) => (p.id === id ? { ...p, ...patch } : p)),
    );
  }

  function addModelsToProvider(id: string, raw: string) {
    const tokens = parseModelTokens(raw);
    if (!tokens.length) return;
    setSettingsProviders((prev) =>
      prev.map((p) =>
        p.id === id
          ? { ...p, models: normalizeModels([...p.models, ...tokens]), modelDraft: "" }
          : p,
      ),
    );
  }

  function removeModelFromProvider(id: string, model: string) {
    setSettingsProviders((prev) =>
      prev.map((p) =>
        p.id === id ? { ...p, models: p.models.filter((m) => m !== model) } : p,
      ),
    );
  }

  function addModelsToNewProvider(raw: string) {
    const tokens = parseModelTokens(raw);
    if (!tokens.length) return;
    setNewProviderModels((prev) => normalizeModels([...prev, ...tokens]));
    setNewProviderModelDraft("");
  }

  function removeModelFromNewProvider(model: string) {
    setNewProviderModels((prev) => prev.filter((m) => m !== model));
  }

  async function saveProvidersDefaultModel() {
    const trimmed = settingsProvidersDefaultModel.trim();
    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({ default_model: trimmed ? trimmed : null });
      await refreshAfterConfigChange();
      pushAlert({
        id: newAlertId("cfg"),
        level: "success",
        title: "Saved",
        message: "Default model updated.",
        autoCloseMs: 2500,
      });
    } catch (err) {
      handleApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function clearProvidersDefaultModel() {
    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({ default_model: null });
      setSettingsProvidersDefaultModel("");
      await refreshAfterConfigChange();
      pushAlert({
        id: newAlertId("cfg"),
        level: "success",
        title: "Saved",
        message: "Default model cleared.",
        autoCloseMs: 2500,
      });
    } catch (err) {
      handleApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function addProvider() {
    const id = newProviderId.trim();
    const baseUrl = newProviderBaseUrl.trim();
    const models = normalizeModels(newProviderModels);
    const apiKey = newProviderApiKey.trim();

    if (!id) {
      pushAlert({ id: newAlertId("cfg"), level: "error", title: "Invalid provider", message: "Provider id is required." });
      return;
    }
    if (!baseUrl) {
      pushAlert({ id: newAlertId("cfg"), level: "error", title: "Invalid provider", message: "Base URL is required." });
      return;
    }

    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({
        upsert: [
          {
            id,
            base_url: baseUrl,
            models,
            api_key: apiKey ? apiKey : undefined,
          },
        ],
      });
      setProvidersPaneSelection(id);
      await loadSettingsProviders();
      await refreshAfterConfigChange();
      setNewProviderId("");
      setNewProviderBaseUrl("");
      setNewProviderModels([]);
      setNewProviderModelDraft("");
      setNewProviderApiKey("");
    } catch (err) {
      handleApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function saveProvider(id: string) {
    const p = settingsProviders.find((v) => v.id === id);
    if (!p) return;
    const baseUrl = p.baseUrl.trim();
    if (!baseUrl) {
      pushAlert({ id: newAlertId("cfg"), level: "error", title: "Invalid provider", message: "Base URL is required." });
      return;
    }
    const models = normalizeModels(p.models);

    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({
        upsert: [{ id, base_url: baseUrl, models }],
      });
      setProviderDraft(id, { baseUrl, models });
      await refreshAfterConfigChange();
    } catch (err) {
      handleApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function updateProviderApiKey(id: string) {
    const p = settingsProviders.find((v) => v.id === id);
    if (!p) return;
    const key = p.apiKeyDraft.trim();
    if (!key) return;

    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({ upsert: [{ id, api_key: key }] });
      setProviderDraft(id, { apiKeyDraft: "", apiKeySet: true });
      await refreshAfterConfigChange();
    } catch (err) {
      handleApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function clearProviderApiKey(id: string) {
    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({ upsert: [{ id, api_key: null }] });
      setProviderDraft(id, { apiKeyDraft: "", apiKeySet: false });
      await refreshAfterConfigChange();
    } catch (err) {
      handleApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function deleteProvider(id: string) {
    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({ delete: [id] });
      const remaining = settingsProviders.filter((p) => p.id !== id);
      setSettingsProviders(remaining);
      setProvidersPaneSelection((prev) => {
        if (prev !== id) return prev;
        return remaining[0]?.id ?? PROVIDERS_PANE_NEW_PROVIDER;
      });
      await refreshAfterConfigChange();
      pushAlert({
        id: newAlertId("cfg"),
        level: "success",
        title: "Deleted",
        message: `Provider "${id}" deleted.`,
        autoCloseMs: 3000,
      });
    } catch (err) {
      handleApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  function parseOptionalPositiveInt(
    label: string,
    raw: string,
  ): number | null | "invalid" {
    const trimmed = raw.trim();
    if (!trimmed) return null;
    const n = Number(trimmed);
    if (!Number.isFinite(n) || !Number.isInteger(n) || n <= 0) {
      pushAlert({
        id: newAlertId("cfg"),
        level: "error",
        title: "Invalid value",
        message: `${label} must be a positive integer.`,
        autoCloseMs: 4000,
      });
      return "invalid";
    }
    return n;
  }

  async function saveRuntimeConfig() {
    const runtime = parseOptionalPositiveInt("runtime.max_steps", runtimeMaxSteps);
    const plan = parseOptionalPositiveInt("agents.plan.max_steps", agentsPlanMaxSteps);
    const general = parseOptionalPositiveInt("agents.general.max_steps", agentsGeneralMaxSteps);
    if (runtime === "invalid" || plan === "invalid" || general === "invalid") return;

    setSettingsSaving(true);
    try {
      await api.patchConfigRuntime({
        runtime_max_steps: runtime,
        agents_plan_max_steps: plan,
        agents_general_max_steps: general,
      });
      await refreshAfterConfigChange();
    } catch (err) {
      handleApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function patchSession(patch: any): Promise<boolean> {
    if (!selectedId) return false;
    try {
      const s = await api.patchSessionSettings(selectedId, patch);
      setSession(s);
      await refreshSessions();
      return true;
    } catch (err) {
      handleApiError(err);
      return false;
    }
  }

  async function saveSelectedSessionDefaults(
    req: { model: boolean; agent?: boolean; mcp: boolean; skills?: boolean },
    onSaving: (saving: boolean) => void,
    message: string,
  ) {
    if (!selectedId) return;
    onSaving(true);
    try {
      await api.saveSessionDefaults(selectedId, req);
      await refreshAfterConfigChange();
      pushAlert({
        id: newAlertId("defaults"),
        level: "success",
        title: "Saved",
        message,
        autoCloseMs: 3000,
      });
    } catch (err) {
      handleApiError(err);
    } finally {
      onSaving(false);
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

  async function forkSessionCopy(sessionId: string, messageId?: string) {
    try {
      const res: any = await api.forkSession(sessionId, messageId);
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

  async function forkSessionFromAssistant(assistantMessageId: string) {
    if (!selectedId) return;
    await forkSessionCopy(selectedId, assistantMessageId);
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
    }, 10_000);
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
    if (isNarrowViewport) return;
    saveSidebarOpen(sidebarOpen);
  }, [sidebarOpen, isNarrowViewport]);

  useEffect(() => {
    saveWorkspacesPaneOpen(workspacesPaneOpen);
  }, [workspacesPaneOpen]);

  useEffect(() => {
    saveSessionsPaneOpen(sessionsPaneOpen);
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
    if (isNarrowViewport) {
      setSessionMenu(null);
      setWorkspaceMenu(null);
      return;
    }
    setSessionActionSheet(null);
    setWorkspaceActionSheet(null);
  }, [isNarrowViewport]);

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
      wsEvents.close();
      wsEvents.resetReconnectAttempt();
      lastEventIdRef.current = 0;
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
  const historyMutable = Boolean(
    selectedSummary &&
      selectedSummary.status.active_run_id == null &&
      selectedSummary.status.queue_len === 0 &&
      selectedSummary.status.run_state === "idle",
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

  const sessionActionSheetSummary = sessionActionSheet
    ? sortedSessions.find((s) => s.id === sessionActionSheet.sessionId) ?? null
    : null;
  const sessionActionSheetLabel =
    sessionActionSheetSummary?.title ??
    sessionActionSheetSummary?.id ??
    sessionActionSheet?.sessionId ??
    "";
  const workspaceActionSheetLabel = workspaceActionSheet?.workspaceRoot
    ? workspaceDisplayName(workspaceActionSheet.workspaceRoot)
    : "";

  const sidebarPaneTopPadding = isNarrowViewport ? "pt-2" : "pt-3";
  const sidebarPane = (
    <div className="flex h-full flex-col">
      {isNarrowViewport ? (
        <div className="flex items-center justify-between px-3 pt-3">
          <div className="text-xs font-semibold text-zinc-700">Menu</div>
          <SheetClose asChild>
            <Button
              variant="ghost"
              size="icon"
              className="h-8 w-8"
              aria-label="Close sidebar"
              title="Close sidebar"
            >
              <X className="h-4 w-4 text-zinc-600" />
            </Button>
          </SheetClose>
        </div>
      ) : null}

      <div className={cn("space-y-1 p-3", sidebarPaneTopPadding)}>
        <Button variant="ghost" className="w-full justify-start gap-2" onClick={onNewSession}>
          <Plus className="h-4 w-4 text-violet-600" />
          New Session
        </Button>
        <Button variant="ghost" className="w-full justify-start gap-2" onClick={openSkills}>
          <Sparkles className="h-4 w-4 text-amber-600" />
          Skills
        </Button>
        <Button variant="ghost" className="w-full justify-start gap-2" onClick={openMcp}>
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
                          <div className="min-w-0 truncate text-xs font-medium text-zinc-800" title={g.root}>
                            {workspaceBasename(g.root)}
                          </div>
                          <div className="shrink-0 text-[10px] text-zinc-500">({g.sessions.length})</div>
                        </button>

                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7"
                          aria-label="Workspace actions"
                          onClick={(e) => {
                            e.preventDefault();
                            e.stopPropagation();
                            const rect = (e.currentTarget as HTMLButtonElement).getBoundingClientRect();
                            openWorkspaceMenuAt(g.root, rect);
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
                            const active = s.id === selectedId;
                            const pinned = pinnedSessionIds.includes(s.id);
                            return (
                              <SessionItemRow
                                key={s.id}
                                summary={s}
                                active={active}
                                pinned={pinned}
                                onSelect={() => selectSession(s.id)}
                                onOpenMenu={openSessionMenuAt}
                              />
                            );
                          })}
                          {!g.sessions.length ? (
                            <div className="px-2 py-2 text-xs text-zinc-500">No sessions</div>
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
                      onClick={() => setWorkspacesVisible((v) => v + LIST_PAGE_SIZE)}
                    >
                      Load more
                    </Button>
                  </div>
                ) : null}

                {!workspaceGroups.length ? (
                  <div className="px-2 py-6 text-center text-sm text-zinc-500">No workspaces</div>
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
                  const active = s.id === selectedId;
                  const pinned = pinnedSessionIds.includes(s.id);
                  return (
                    <SessionItemRow
                      key={s.id}
                      summary={s}
                      active={active}
                      pinned={pinned}
                      onSelect={() => selectSession(s.id)}
                      onOpenMenu={openSessionMenuAt}
                    />
                  );
                })}

                {canLoadMoreSessions ? (
                  <div className="px-2 py-2">
                    <Button
                      variant="ghost"
                      className="w-full justify-center text-xs"
                      onClick={() => setSessionsVisible((v) => v + LIST_PAGE_SIZE)}
                    >
                      Load more
                    </Button>
                  </div>
                ) : null}
                {!tmpSessions.length ? (
                  <div className="px-2 py-6 text-center text-sm text-zinc-500">No sessions</div>
                ) : null}
              </div>
            </div>
          ) : null}
        </div>
      </div>

      <Separator />

      <div className="p-3">
        <Button variant="ghost" className="w-full justify-start gap-2" onClick={openSettings}>
          <Settings className="h-4 w-4 text-blue-600" />
          Settings
        </Button>
      </div>
    </div>
  );

  return (
    <div className="h-dvh w-full bg-zinc-50 text-zinc-900">
      <AlertStack items={alerts} onClose={closeAlert} />
      <div className="flex h-full overflow-hidden">
        {isNarrowViewport ? (
          <Sheet open={sidebarOpen} onOpenChange={setSidebarOpen}>
            <SheetContent side="left" showClose={false} className="border-r border-zinc-200 bg-zinc-50">
              {sidebarPane}
            </SheetContent>
          </Sheet>
        ) : sidebarOpen ? (
          <aside className="flex w-[280px] flex-col border-r border-zinc-200 bg-zinc-50">
            {sidebarPane}
          </aside>
        ) : null}

        <main className="flex min-w-0 flex-1 flex-col">
          <div className="flex flex-col gap-3 border-b border-zinc-200 bg-white px-4 py-3 lg:flex-row lg:items-start lg:justify-between">
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
                <div className="mt-0.5 flex flex-wrap items-center gap-2 text-xs text-zinc-600">
                  {selectedBadge ? (
                    <Badge variant={selectedBadge.variant}>{selectedBadge.label}</Badge>
                  ) : (
                    <Badge variant="idle">idle</Badge>
                  )}
                  {session ? (
                    <button
                      type="button"
                      className="truncate hover:text-zinc-800 active:opacity-80"
                      title={session.settings.model_id}
                      onClick={() => copyWithToast(session.settings.model_id, "Model id")}
                    >
                      {session.settings.agent} · {modelLabel(session.settings.model_id)}
                    </button>
                  ) : null}
                </div>
              </div>
            </div>

            {session ? (
              <div className="grid w-full grid-cols-1 gap-2 sm:grid-cols-2 lg:w-auto lg:flex lg:flex-wrap lg:items-center lg:justify-end">
                <div className="flex items-center gap-2">
                  <span className="shrink-0 text-xs text-zinc-600">Agent</span>
                  <select
                    className="h-8 min-w-0 flex-1 rounded-md border border-zinc-200 bg-white px-2 text-xs lg:w-[160px] lg:flex-none"
                    value={session.settings.agent}
                    title={session.settings.agent}
                    aria-label="Agent"
                    onChange={(e) => patchSession({ agent: e.target.value })}
                  >
                    {agentOptions.map((a) => (
                      <option key={a} value={a}>
                        {a}
                      </option>
                    ))}
                  </select>
                </div>

                <div className="flex items-center gap-2">
                  <span className="shrink-0 text-xs text-zinc-600">Model</span>
                  <select
                    className="h-8 min-w-0 flex-1 rounded-md border border-zinc-200 bg-white px-2 text-xs lg:w-[240px] lg:flex-none"
                    value={session.settings.model_id}
                    title={session.settings.model_id}
                    aria-label="Model"
                    onChange={(e) => patchSession({ model_id: e.target.value })}
                  >
                    {modelOptions.map((m) => (
                      <option key={m} value={m}>
                        {modelLabel(m)}
                      </option>
                    ))}
                  </select>
                </div>

                <div className="flex min-w-0 items-center justify-end gap-2 sm:col-span-2">
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8"
                    disabled={modelDefaultsSaving}
                    title="Set agent & model defaults for new sessions"
                    aria-label="Set agent & model defaults for new sessions"
                    onClick={() =>
                      saveSelectedSessionDefaults(
                        { model: true, agent: true, mcp: false },
                        setModelDefaultsSaving,
                        "Saved current session agent and model as the defaults for new sessions.",
                      )
                    }
                  >
                    <Star className="h-4 w-4 text-yellow-500 fill-yellow-500" />
                  </Button>

                  <div className="flex min-w-0 max-w-full items-center gap-2 rounded-md px-2 py-1 lg:max-w-[420px]">
                    <span className="hidden shrink-0 text-xs text-zinc-600 sm:inline">
                      workspace
                    </span>
                    <button
                      type="button"
                      className="min-w-0 truncate font-mono text-xs text-zinc-800 hover:text-zinc-900 active:opacity-80"
                      title={session.settings.workspace_root ?? ""}
                      onClick={() =>
                        copyWithToast(session.settings.workspace_root ?? "", "Workspace path")
                      }
                    >
                      {workspaceDisplayName(session.settings.workspace_root ?? "")}
                    </button>
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
                    <span className="hidden sm:inline">Add folder</span>
                  </Button>
                </div>
              </div>
            ) : null}
          </div>

          <div
            ref={chatScrollRef}
            onScroll={updateIsAtBottom}
            className="flex-1 overflow-auto bg-zinc-50 px-3 py-4 sm:px-4"
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
                    onEditUser={openEditDialog}
                    onRegenerateAssistant={regenerateAssistant}
                    historyMutable={historyMutable}
                  />
                ))}

                {stream.thinking ? (
                  <div className="flex justify-start">
                    <details className="relative w-full max-w-[92%] rounded-2xl border border-zinc-200 bg-zinc-50 px-4 py-2 sm:max-w-[78%]">
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
                      className={`${hasMermaidFence(stream.assistant) ? "w-full max-w-[92%]" : "max-w-[92%] sm:max-w-[78%]"} rounded-2xl bg-zinc-50 px-4 py-2 text-sm text-zinc-900`}
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
            <div className="fixed bottom-24 right-3 z-40 sm:right-6">
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

          <div className="border-t border-zinc-200 bg-white px-3 pt-3 pb-[calc(0.75rem+env(safe-area-inset-bottom))] sm:px-4">
            <div className="relative mx-auto w-full max-w-4xl">
              <div className="flex min-w-0 items-center gap-2 rounded-3xl border border-zinc-200 bg-white px-4 py-2 shadow-sm hover:border-zinc-300 focus-within:border-blue-300 focus-within:ring-2 focus-within:ring-blue-500/20">
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

              <div className="absolute left-full top-1/2 hidden -translate-y-1/2 translate-x-2 items-center gap-1 rounded-3xl border border-zinc-200 bg-white px-2 py-2 shadow-sm xl:flex">
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
        </main>
      </div>

      <Dialog open={settingsOpen} onOpenChange={setSettingsOpen}>
        <DialogContent className="md:w-[min(1080px,calc(100vw-24px))] md:max-w-none">
          <DialogHeader>
            <DialogTitle>Settings</DialogTitle>
            <DialogDescription className="truncate">
              Global configuration (kiliax.yaml)
            </DialogDescription>
          </DialogHeader>
          <div className="mb-3 flex items-center gap-2">
            <div className="inline-flex rounded-md border border-zinc-200 bg-white p-1">
              <button
                className={cn(
                  "rounded px-3 py-1 text-xs",
                  settingsTab === "providers"
                    ? "bg-blue-50 text-blue-700"
                    : "text-zinc-700 hover:bg-zinc-50",
                )}
                onClick={() => setSettingsTab("providers")}
              >
                Providers
              </button>
              <button
                className={cn(
                  "rounded px-3 py-1 text-xs",
                  settingsTab === "agents"
                    ? "bg-blue-50 text-blue-700"
                    : "text-zinc-700 hover:bg-zinc-50",
                )}
                onClick={() => setSettingsTab("agents")}
              >
                Agents
              </button>
              <button
                className={cn(
                  "rounded px-3 py-1 text-xs",
                  settingsTab === "yaml"
                    ? "bg-blue-50 text-blue-700"
                    : "text-zinc-700 hover:bg-zinc-50",
                )}
                onClick={() => {
                  setSettingsTab("yaml");
                  ensureConfigLoaded().catch(handleApiError);
                }}
              >
                Raw YAML
              </button>
            </div>
            <div className="flex-1" />
            <Button
              variant="outline"
              size="icon"
              className="h-8 w-8"
              aria-label="Reload settings"
              title="Reload settings"
              disabled={settingsLoading || settingsSaving || savingConfig}
              onClick={async () => {
                setSettingsLoading(true);
                try {
                  await Promise.all([loadSettingsProviders(), loadSettingsRuntime()]);
                  if (settingsTab === "yaml") {
                    await ensureConfigLoaded(true);
                  }
                } catch (err) {
                  handleApiError(err);
                } finally {
                  setSettingsLoading(false);
                }
              }}
            >
              <RefreshCcw className="h-4 w-4 text-zinc-600" />
            </Button>
          </div>

          {settingsLoading ? (
            <div className="py-10 text-center text-sm text-zinc-500">Loading…</div>
          ) : settingsTab === "providers" ? (
            <div className="flex h-[min(600px,72vh)] flex-col gap-3 md:flex-row">
              <div className="flex h-[min(240px,30vh)] w-full shrink-0 flex-col overflow-hidden rounded-lg border border-zinc-200 bg-white md:h-full md:w-80">
                <div className="flex items-center justify-between border-b border-zinc-200 px-3 py-2">
                  <div className="text-xs font-semibold text-zinc-700">Providers</div>
                  <div className="text-xs text-zinc-500">{settingsProviders.length}</div>
                </div>

                <div className="min-h-0 flex-1 overflow-auto p-2">
                  <div className="space-y-1">
                    <button
                      className={cn(
                        "w-full rounded-md border border-transparent px-2 py-2 text-left hover:border-zinc-200 hover:bg-zinc-50",
                        providersPaneSelection === PROVIDERS_PANE_DEFAULT_MODEL
                          ? "border-blue-200 bg-blue-50"
                          : "",
                      )}
                      onClick={() => setProvidersPaneSelection(PROVIDERS_PANE_DEFAULT_MODEL)}
                    >
                      <div className="text-sm font-medium text-zinc-900">Default model</div>
                      <div className="mt-0.5 truncate text-xs text-zinc-500">
                        {settingsProvidersDefaultModel.trim() ? (
                          <span className="font-mono">{settingsProvidersDefaultModel.trim()}</span>
                        ) : (
                          "Not set"
                        )}
                      </div>
                    </button>

                    <button
                      className={cn(
                        "w-full rounded-md border border-transparent px-2 py-2 text-left hover:border-zinc-200 hover:bg-zinc-50",
                        providersPaneSelection === PROVIDERS_PANE_NEW_PROVIDER
                          ? "border-blue-200 bg-blue-50"
                          : "",
                      )}
                      onClick={() => setProvidersPaneSelection(PROVIDERS_PANE_NEW_PROVIDER)}
                    >
                      <div className="flex items-center gap-2 text-sm font-medium text-zinc-900">
                        <Plus className="h-4 w-4 text-violet-600" />
                        Add provider
                      </div>
                      <div className="mt-0.5 truncate text-xs text-zinc-500">
                        Create a new OpenAI-compatible provider.
                      </div>
                    </button>
                  </div>

                  <div className="my-2 border-t border-zinc-200" />

                  {settingsProviders.length ? (
                    <div className="space-y-1">
                      {settingsProviders.map((p) => (
                        <button
                          key={p.id}
                          className={cn(
                            "w-full rounded-md border border-transparent px-2 py-2 text-left hover:border-zinc-200 hover:bg-zinc-50",
                            providersPaneSelection === p.id ? "border-blue-200 bg-blue-50" : "",
                          )}
                          onClick={() => setProvidersPaneSelection(p.id)}
                        >
                          <div className="flex items-start justify-between gap-2">
                            <div className="min-w-0">
                              <div className="truncate text-sm font-medium text-zinc-900">
                                {p.id}
                              </div>
                              <div className="mt-0.5 truncate text-xs text-zinc-500">
                                {p.baseUrl || "—"}
                              </div>
                            </div>
                            <div className="flex shrink-0 flex-col items-end gap-1">
                              <Badge
                                variant={p.apiKeySet ? "done" : "idle"}
                                className="px-2 py-0.5 text-[11px]"
                              >
                                key
                              </Badge>
                              <div className="text-[11px] text-zinc-500">
                                {p.models.length} models
                              </div>
                            </div>
                          </div>
                        </button>
                      ))}
                    </div>
                  ) : (
                    <div className="px-2 py-6 text-center text-sm text-zinc-500">
                      No providers
                    </div>
                  )}
                </div>
              </div>

              <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-lg border border-zinc-200 bg-white">
                <div className="flex items-center justify-between border-b border-zinc-200 px-4 py-2">
                  <div className="min-w-0">
                    <div className="truncate text-sm font-semibold text-zinc-900">
                      {providersPaneSelection === PROVIDERS_PANE_DEFAULT_MODEL
                        ? "Default model"
                        : providersPaneSelection === PROVIDERS_PANE_NEW_PROVIDER
                          ? "Add provider"
                          : providersPaneSelectedProvider?.id ?? "Provider"}
                    </div>
                    <div className="truncate text-xs text-zinc-500">
                      {providersPaneSelection === PROVIDERS_PANE_DEFAULT_MODEL
                        ? "Used for new sessions when not overridden."
                        : providersPaneSelection === PROVIDERS_PANE_NEW_PROVIDER
                          ? "Base URL, models, and API key."
                          : providersPaneSelectedProvider?.baseUrl ??
                            "Select a provider on the left."}
                    </div>
                  </div>
                  {settingsSaving ? (
                    <div className="text-xs text-zinc-500">Saving…</div>
                  ) : null}
                </div>

                <div className="min-h-0 flex-1 overflow-auto p-4">
                  {providersPaneSelection === PROVIDERS_PANE_DEFAULT_MODEL ? (
                    <div className="space-y-3">
                      <div className="text-xs text-zinc-600">Model id</div>
                      <Input
                        className="font-mono text-xs"
                        list="kiliax-model-suggestions"
                        placeholder="provider/model"
                        value={settingsProvidersDefaultModel}
                        onChange={(e) => setSettingsProvidersDefaultModel(e.target.value)}
                      />
                      <div className="flex justify-end gap-2">
                        <Button
                          variant="outline"
                          disabled={settingsSaving || !settingsProvidersDefaultModel.trim()}
                          onClick={clearProvidersDefaultModel}
                        >
                          Clear
                        </Button>
                        <Button onClick={saveProvidersDefaultModel} disabled={settingsSaving}>
                          Save
                        </Button>
                      </div>
                      <div className="text-xs text-zinc-500">
                        Example: <span className="font-mono">openai/gpt-4o-mini</span>
                      </div>
                    </div>
                  ) : providersPaneSelection === PROVIDERS_PANE_NEW_PROVIDER ? (
                    <div className="space-y-4">
                      <div className="grid gap-3 sm:grid-cols-2">
                        <div>
                          <div className="text-xs text-zinc-600">Provider id</div>
                          <Input
                            placeholder="openai"
                            value={newProviderId}
                            onChange={(e) => setNewProviderId(e.target.value)}
                          />
                        </div>
                        <div>
                          <div className="text-xs text-zinc-600">Base URL</div>
                          <Input
                            placeholder="https://api.openai.com/v1"
                            value={newProviderBaseUrl}
                            onChange={(e) => setNewProviderBaseUrl(e.target.value)}
                          />
                        </div>
                      </div>

                      <div>
                        <div className="flex items-center justify-between">
                          <div className="text-xs text-zinc-600">Models</div>
                          <div className="text-xs text-zinc-500">{newProviderModels.length} total</div>
                        </div>
                        <div className="mt-2 flex flex-wrap gap-2">
                          {newProviderModels.length ? (
                            newProviderModels.map((m) => (
                              <div
                                key={m}
                                className="flex min-w-0 max-w-full items-center gap-1 rounded-full border border-zinc-200 bg-zinc-50 px-2 py-1 text-xs"
                              >
                                <span className="max-w-[240px] truncate font-mono" title={m}>
                                  {m}
                                </span>
                                <button
                                  type="button"
                                  className="rounded-full p-0.5 text-zinc-500 hover:bg-zinc-200"
                                  aria-label={`Remove model ${m}`}
                                  onClick={() => removeModelFromNewProvider(m)}
                                >
                                  <X className="h-3 w-3" />
                                </button>
                              </div>
                            ))
                          ) : (
                            <div className="text-xs text-zinc-500">No models</div>
                          )}
                        </div>
                        <div className="mt-2 flex gap-2">
                          <Input
                            className="font-mono text-xs"
                            placeholder="Add model"
                            value={newProviderModelDraft}
                            onChange={(e) => setNewProviderModelDraft(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter") {
                                e.preventDefault();
                                addModelsToNewProvider(newProviderModelDraft);
                              }
                            }}
                          />
                          <Button
                            variant="outline"
                            disabled={settingsSaving || !newProviderModelDraft.trim()}
                            onClick={() => addModelsToNewProvider(newProviderModelDraft)}
                          >
                            Add
                          </Button>
                        </div>
                      </div>

                      <div>
                        <div className="text-xs text-zinc-600">API key (optional)</div>
                        <Input
                          type="password"
                          placeholder="sk-…"
                          value={newProviderApiKey}
                          onChange={(e) => setNewProviderApiKey(e.target.value)}
                        />
                      </div>

                      <div className="flex justify-end">
                        <Button onClick={addProvider} disabled={settingsSaving}>
                          Add provider
                        </Button>
                      </div>
                    </div>
                  ) : providersPaneSelectedProvider ? (
                    <div className="space-y-4">
                      <div>
                        <div className="text-xs text-zinc-600">Base URL</div>
                        <Input
                          value={providersPaneSelectedProvider.baseUrl}
                          onChange={(e) =>
                            setProviderDraft(providersPaneSelectedProvider.id, {
                              baseUrl: e.target.value,
                            })
                          }
                        />
                      </div>

                      <div>
                        <div className="flex items-center justify-between">
                          <div className="text-xs text-zinc-600">Models</div>
                          <div className="text-xs text-zinc-500">
                            {providersPaneSelectedProvider.models.length} total
                          </div>
                        </div>
                        <div className="mt-2 flex flex-wrap gap-2">
                          {providersPaneSelectedProvider.models.length ? (
                            providersPaneSelectedProvider.models.map((m) => (
                              <div
                                key={m}
                                className="flex min-w-0 max-w-full items-center gap-1 rounded-full border border-zinc-200 bg-zinc-50 px-2 py-1 text-xs"
                              >
                                <span className="max-w-[240px] truncate font-mono" title={m}>
                                  {m}
                                </span>
                                <button
                                  type="button"
                                  className="rounded-full p-0.5 text-zinc-500 hover:bg-zinc-200 disabled:opacity-50"
                                  aria-label={`Remove model ${m}`}
                                  disabled={settingsSaving}
                                  onClick={() =>
                                    removeModelFromProvider(
                                      providersPaneSelectedProvider.id,
                                      m,
                                    )
                                  }
                                >
                                  <X className="h-3 w-3" />
                                </button>
                              </div>
                            ))
                          ) : (
                            <div className="text-xs text-zinc-500">No models</div>
                          )}
                        </div>
                        <div className="mt-2 flex gap-2">
                          <Input
                            className="font-mono text-xs"
                            placeholder="Add model"
                            value={providersPaneSelectedProvider.modelDraft}
                            onChange={(e) =>
                              setProviderDraft(providersPaneSelectedProvider.id, {
                                modelDraft: e.target.value,
                              })
                            }
                            onKeyDown={(e) => {
                              if (e.key === "Enter") {
                                e.preventDefault();
                                addModelsToProvider(
                                  providersPaneSelectedProvider.id,
                                  providersPaneSelectedProvider.modelDraft,
                                );
                              }
                            }}
                          />
                          <Button
                            variant="outline"
                            disabled={
                              settingsSaving ||
                              !providersPaneSelectedProvider.modelDraft.trim()
                            }
                            onClick={() =>
                              addModelsToProvider(
                                providersPaneSelectedProvider.id,
                                providersPaneSelectedProvider.modelDraft,
                              )
                            }
                          >
                            Add
                          </Button>
                        </div>
                      </div>

                      <div>
                        <div className="flex items-center justify-between">
                          <div className="text-xs text-zinc-600">API key</div>
                          <div className="text-xs text-zinc-500">
                            {providersPaneSelectedProvider.apiKeySet ? "set" : "not set"}
                          </div>
                        </div>
                        <div className="mt-2 flex items-end gap-2">
                          <div className="min-w-0 flex-1">
                            <Input
                              type="password"
                              placeholder="Enter new API key"
                              value={providersPaneSelectedProvider.apiKeyDraft}
                              onChange={(e) =>
                                setProviderDraft(providersPaneSelectedProvider.id, {
                                  apiKeyDraft: e.target.value,
                                })
                              }
                            />
                          </div>
                          <Button
                            onClick={() =>
                              updateProviderApiKey(providersPaneSelectedProvider.id)
                            }
                            disabled={
                              settingsSaving ||
                              !providersPaneSelectedProvider.apiKeyDraft.trim()
                            }
                          >
                            Update
                          </Button>
                          <Button
                            variant="outline"
                            onClick={() =>
                              clearProviderApiKey(providersPaneSelectedProvider.id)
                            }
                            disabled={settingsSaving || !providersPaneSelectedProvider.apiKeySet}
                          >
                            Clear
                          </Button>
                        </div>
                        <div className="mt-1 text-xs text-zinc-500">
                          Keys are stored in <span className="font-mono">kiliax.yaml</span> and are not shown again.
                        </div>
                      </div>

                      <div className="flex items-center justify-between">
                        <Button
                          variant="outline"
                          disabled={settingsSaving}
                          onClick={() => loadSettingsProviders().catch(handleApiError)}
                        >
                          Revert
                        </Button>
                        <div className="flex gap-2">
                          <Button
                            variant="outline"
                            className="border-rose-200 text-rose-700 hover:bg-rose-50"
                            disabled={settingsSaving}
                            onClick={() =>
                              setProviderDeleteConfirm({
                                providerId: providersPaneSelectedProvider.id,
                              })
                            }
                          >
                            Delete
                          </Button>
                          <Button
                            disabled={settingsSaving}
                            onClick={() => saveProvider(providersPaneSelectedProvider.id)}
                          >
                            Save
                          </Button>
                        </div>
                      </div>
                    </div>
                  ) : (
                    <div className="py-10 text-center text-sm text-zinc-500">
                      Select a provider
                    </div>
                  )}
                </div>
              </div>

              <datalist id="kiliax-model-suggestions">
                {defaultModelSuggestions.map((m) => (
                  <option key={m} value={m} />
                ))}
              </datalist>
            </div>
          ) : settingsTab === "agents" ? (
            <div className="space-y-3">
              <div className="rounded-md border border-zinc-200 bg-white p-3">
                <div className="text-sm font-medium text-zinc-900">Max steps</div>
                <div className="mt-2 grid grid-cols-1 gap-2 sm:grid-cols-3">
                  <div>
                    <div className="text-xs text-zinc-600">runtime.max_steps</div>
                    <Input
                      placeholder="default: 1024"
                      value={runtimeMaxSteps}
                      onChange={(e) => setRuntimeMaxSteps(e.target.value)}
                    />
                  </div>
                  <div>
                    <div className="text-xs text-zinc-600">agents.plan.max_steps</div>
                    <Input
                      placeholder="optional"
                      value={agentsPlanMaxSteps}
                      onChange={(e) => setAgentsPlanMaxSteps(e.target.value)}
                    />
                  </div>
                  <div>
                    <div className="text-xs text-zinc-600">agents.general.max_steps</div>
                    <Input
                      placeholder="optional"
                      value={agentsGeneralMaxSteps}
                      onChange={(e) => setAgentsGeneralMaxSteps(e.target.value)}
                    />
                  </div>
                </div>
                <div className="mt-2 text-xs text-zinc-500">
                  Leave blank to use defaults.
                </div>
                <div className="mt-3 flex justify-end gap-2">
                  <Button
                    variant="outline"
                    disabled={settingsSaving}
                    onClick={() => loadSettingsRuntime().catch(handleApiError)}
                  >
                    Reload
                  </Button>
                  <Button onClick={saveRuntimeConfig} disabled={settingsSaving}>
                    Save
                  </Button>
                </div>
              </div>
            </div>
          ) : (
            <div className="space-y-2">
              <div className="text-xs text-zinc-600">
                Path: <span className="font-mono">{configPath || "kiliax.yaml"}</span>
              </div>
              <div className="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800">
                Warning: raw config may include secrets.
              </div>
              <Textarea
                className="h-[420px] font-mono text-xs"
                value={configYaml}
                onChange={(e) => setConfigYaml(e.target.value)}
              />
              <div className="flex justify-end gap-2">
                <Button
                  variant="outline"
                  onClick={() => {
                    ensureConfigLoaded(true).catch(handleApiError);
                  }}
                  disabled={savingConfig}
                >
                  Reload
                </Button>
                <Button onClick={saveConfig} disabled={savingConfig || !configLoaded}>
                  {savingConfig ? "Saving…" : "Save"}
                </Button>
              </div>
            </div>
          )}
        </DialogContent>
      </Dialog>

      <Dialog open={skillsOpen} onOpenChange={setSkillsOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Skills</DialogTitle>
            <DialogDescription>
              {session ? "Current session skills" : "Select a session to edit skills"}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            {skillsLoadErrors.length ? (
              <div className="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800">
                <div className="font-medium">Some skills failed to load</div>
                <div className="mt-1 max-h-[180px] space-y-2 overflow-auto pr-1">
                  {skillsLoadErrors.slice(0, 6).map((e) => (
                    <div key={`${e.id}:${e.path}`} className="rounded-md border border-amber-200 bg-white/70 px-2 py-1">
                      <div className="font-mono text-xs text-amber-900">{e.id}</div>
                      <div className="mt-0.5 break-all font-mono text-[11px] text-amber-700">
                        {e.path}
                      </div>
                      <div className="mt-0.5 whitespace-pre-wrap break-words text-[11px] text-amber-900">
                        {e.error}
                      </div>
                    </div>
                  ))}
                  {skillsLoadErrors.length > 6 ? (
                    <div className="text-amber-700">
                      +{skillsLoadErrors.length - 6} more…
                    </div>
                  ) : null}
                </div>
              </div>
            ) : null}
            <label className="flex items-center justify-between rounded-md border border-zinc-200 bg-white px-3 py-2">
              <div className="text-sm text-zinc-900">Enable by default</div>
              <input
                type="checkbox"
                checked={skillsDefaultEnable}
                disabled={skillsSaving || !session}
                onChange={async (e) => {
                  const next = e.target.checked;
                  const prev = skillsDefaultEnable;
                  setSkillsDefaultEnable(next);
                  setSkillsSaving(true);
                  try {
                    const ok = await patchSession({ skills: { default_enable: next } });
                    if (!ok) setSkillsDefaultEnable(prev);
                  } finally {
                    setSkillsSaving(false);
                  }
                }}
              />
            </label>

            <div className="max-h-[360px] overflow-auto rounded-md border border-zinc-200">
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
                          disabled={skillsSaving || !session}
                          onChange={async (e) => {
                            if (!session) return;
                            const next = e.target.checked;
                            const prev = skillsOverrides[s.id];
                            setSkillsOverrides((o) => ({ ...o, [s.id]: next }));
                            setSkillsSaving(true);
                            try {
                              const ok = await patchSession({
                                skills: { overrides: [{ id: s.id, enable: next }] },
                              });
                              if (!ok) {
                                setSkillsOverrides((o) => {
                                  const copy = { ...o };
                                  if (prev === undefined) delete copy[s.id];
                                  else copy[s.id] = prev;
                                  return copy;
                                });
                              }
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

            <div className="flex justify-end">
              <Button
                variant="outline"
                size="sm"
                disabled={!session || skillsDefaultsSaving}
                onClick={() =>
                  saveSelectedSessionDefaults(
                    { model: false, mcp: false, skills: true },
                    setSkillsDefaultsSaving,
                    "Saved current session skills as the default.",
                  )
                }
              >
                Save skills defaults
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>

      <Dialog open={mcpOpen} onOpenChange={setMcpOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>MCP</DialogTitle>
            <DialogDescription>Current session MCP servers</DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            {(session?.mcp_status ?? capabilities?.mcp_servers ?? []).map((s) => (
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
                  disabled={mcpSaving || !session}
                  onChange={async (e) => {
                    if (!session) return;
                    const next = e.target.checked;
                    setMcpSaving(true);
                    try {
                      await patchSession({
                        mcp: { servers: [{ id: s.id, enable: next }] },
                      });
                    } catch (err) {
                      handleApiError(err);
                    } finally {
                      setMcpSaving(false);
                    }
                  }}
                />
              </label>
            ))}
            <div className="flex justify-end">
              <Button
                variant="outline"
                size="sm"
                disabled={!session || mcpDefaultsSaving}
                onClick={() =>
                  saveSelectedSessionDefaults(
                    { model: false, mcp: true },
                    setMcpDefaultsSaving,
                    "Saved current session MCP enablement as the default.",
                  )
                }
              >
                Save MCP defaults
              </Button>
            </div>
            {!(session?.mcp_status ?? capabilities?.mcp_servers)?.length ? (
              <div className="text-center text-sm text-zinc-500">No MCP servers</div>
            ) : null}
          </div>
        </DialogContent>
      </Dialog>

      {isNarrowViewport ? (
        <ActionSheet
          open={Boolean(sessionActionSheet)}
          onOpenChange={(open) => !open && setSessionActionSheet(null)}
          title="Session actions"
          description={sessionActionSheetLabel}
        >
          <div className="space-y-2">
            <button
              type="button"
              className="flex w-full items-center gap-3 rounded-md border border-zinc-200 bg-white px-3 py-3 text-left text-base text-zinc-900 active:opacity-80"
              onClick={() => {
                const id = sessionActionSheet?.sessionId;
                if (!id) return;
                setSessionActionSheet(null);
                forkSessionCopy(id);
              }}
            >
              <GitFork className="h-5 w-5 text-violet-600" />
              Fork
            </button>
            <button
              type="button"
              className="flex w-full items-center gap-3 rounded-md border border-zinc-200 bg-white px-3 py-3 text-left text-base text-zinc-900 active:opacity-80"
              onClick={() => {
                const id = sessionActionSheet?.sessionId;
                if (!id) return;
                togglePinnedSession(id);
                setSessionActionSheet(null);
              }}
            >
              <Pin className="h-5 w-5 text-violet-600" />
              {sessionActionSheet?.sessionId && pinnedSessionIds.includes(sessionActionSheet.sessionId)
                ? "Unpin"
                : "Pin"}
            </button>
            <button
              type="button"
              className="flex w-full items-center gap-3 rounded-md border border-rose-200 bg-white px-3 py-3 text-left text-base text-rose-700 active:opacity-80"
              onClick={() => {
                const id = sessionActionSheet?.sessionId;
                if (!id) return;
                setDeleteConfirm({ sessionId: id });
                setSessionActionSheet(null);
              }}
            >
              <Trash2 className="h-5 w-5" />
              Delete
            </button>
            <Button variant="outline" className="w-full" onClick={() => setSessionActionSheet(null)}>
              Cancel
            </Button>
          </div>
        </ActionSheet>
      ) : null}

      {isNarrowViewport ? (
        <ActionSheet
          open={Boolean(workspaceActionSheet)}
          onOpenChange={(open) => !open && setWorkspaceActionSheet(null)}
          title="Workspace actions"
          description={workspaceActionSheetLabel}
        >
          <div className="space-y-2">
            <button
              type="button"
              className="flex w-full items-center gap-3 rounded-md border border-zinc-200 bg-white px-3 py-3 text-left text-base text-zinc-900 active:opacity-80"
              onClick={() => {
                const root = workspaceActionSheet?.workspaceRoot;
                if (!root) return;
                togglePinnedWorkspace(root);
                setWorkspaceActionSheet(null);
              }}
            >
              <Pin className="h-5 w-5 text-violet-600" />
              {workspaceActionSheet?.workspaceRoot && pinnedWorkspaceRoots.includes(workspaceActionSheet.workspaceRoot)
                ? "Unpin"
                : "Pin"}
            </button>
            <button
              type="button"
              className="flex w-full items-center gap-3 rounded-md border border-rose-200 bg-white px-3 py-3 text-left text-base text-rose-700 active:opacity-80"
              onClick={() => {
                const root = workspaceActionSheet?.workspaceRoot;
                if (!root) return;
                setWorkspaceDeleteConfirm({ workspaceRoot: root });
                setWorkspaceActionSheet(null);
              }}
            >
              <Trash2 className="h-5 w-5" />
              Delete
            </button>
            <Button variant="outline" className="w-full" onClick={() => setWorkspaceActionSheet(null)}>
              Cancel
            </Button>
          </div>
        </ActionSheet>
      ) : null}

      {sessionMenu && !isNarrowViewport ? (
        <div
          ref={sessionMenuRef}
          style={{ left: sessionMenu.x, top: sessionMenu.y }}
          className="fixed z-50 mt-1 w-44 -translate-x-full rounded-md border border-zinc-200 bg-white p-1 shadow-lg"
        >
          <button
            className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-zinc-800 hover:bg-zinc-100"
            onClick={() => {
              const id = sessionMenu.sessionId;
              setSessionMenu(null);
              forkSessionCopy(id);
            }}
          >
            <GitFork className="h-4 w-4 text-violet-600" />
            Fork
          </button>
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

      {workspaceMenu && !isNarrowViewport ? (
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
          {isTmpWorkspaceRoot(deleteSessionSummary?.settings.workspace_root ?? "") ? (
            <div className="mt-3 text-sm text-zinc-600">
              This also deletes the temporary workspace directory on disk.
            </div>
          ) : null}
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
            {isTmpWorkspaceRoot(workspaceDeleteConfirm?.workspaceRoot ?? "") ? (
              <>This deletes all sessions and removes the temporary workspace directory on disk.</>
            ) : (
              <>This deletes all sessions under this workspace (directory is not removed).</>
            )}
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

      <Dialog
        open={Boolean(providerDeleteConfirm)}
        onOpenChange={(open) => !open && setProviderDeleteConfirm(null)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete provider?</DialogTitle>
            <DialogDescription className="truncate">
              {providerDeleteConfirm?.providerId ?? ""}
            </DialogDescription>
          </DialogHeader>
          <div className="mt-3 text-sm text-zinc-600">
            This updates global config and may affect sessions using this provider.
          </div>
          <div className="mt-3 flex justify-end gap-2">
            <Button variant="outline" onClick={() => setProviderDeleteConfirm(null)}>
              Cancel
            </Button>
            <Button
              className="bg-red-600 text-zinc-50 hover:bg-red-500"
              disabled={settingsSaving}
              onClick={async () => {
                const id = providerDeleteConfirm?.providerId;
                if (!id) return;
                setProviderDeleteConfirm(null);
                await deleteProvider(id);
              }}
            >
              Delete
            </Button>
          </div>
        </DialogContent>
      </Dialog>

      <Dialog
        open={editMessageOpen}
        onOpenChange={(open) => {
          if (open) {
            setEditMessageOpen(true);
            return;
          }
          setEditMessageOpen(false);
          setEditMessageId(null);
          setEditDraft("");
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Edit message</DialogTitle>
            <DialogDescription>Edits history and regenerates from here.</DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <Textarea
              value={editDraft}
              onChange={(e) => setEditDraft(e.target.value)}
              className="min-h-[140px] resize-none"
              placeholder="Update the user message…"
            />
            <div className="flex justify-end gap-2">
              <Button
                variant="outline"
                onClick={() => {
                  setEditMessageOpen(false);
                  setEditMessageId(null);
                  setEditDraft("");
                }}
                disabled={editSaving}
              >
                Cancel
              </Button>
              <Button
                onClick={saveEditAndSend}
                disabled={!historyMutable || editSaving || !editDraft.trim() || !editMessageId}
              >
                {editSaving ? "Saving…" : "Save & Send"}
              </Button>
            </div>
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
