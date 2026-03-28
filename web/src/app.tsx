import React, { useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, wsUrl } from "@/lib/api";
import { hrefToSession, navigate, useRoute } from "@/lib/router";
import type {
  Capabilities,
  Message,
  Session,
  SessionLastOutcome,
  SessionRunState,
  SessionSummary,
  SkillSummary,
  ToolCall,
} from "@/lib/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { Textarea } from "@/components/ui/textarea";

type PendingMessage = {
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

function sortSessions(items: SessionSummary[]): SessionSummary[] {
  return [...items].sort((a, b) => {
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
          <pre className="mt-2 overflow-auto rounded bg-zinc-50 p-2 text-xs text-zinc-800">
            {c.arguments}
          </pre>
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
        <div className="max-w-[78%] rounded-2xl border border-zinc-200 bg-white px-4 py-2 text-sm text-zinc-900">
          {msg.content ? (
            <div className="whitespace-pre-wrap">{msg.content}</div>
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
        <pre className="mt-2 overflow-auto rounded bg-white p-2 text-xs text-zinc-800">
          {msg.content}
        </pre>
      </details>
    </div>
  );
}

function EmptyState() {
  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="text-center">
        <div className="text-xl font-semibold text-zinc-900">Let&apos;s build</div>
        <div className="mt-1 text-sm text-zinc-600">Create or select a session</div>
      </div>
    </div>
  );
}

export default function App() {
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
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

  const [composerText, setComposerText] = useState("");
  const [savingConfig, setSavingConfig] = useState(false);
  const [configYaml, setConfigYaml] = useState("");
  const [configPath, setConfigPath] = useState("");

  const [skillsOpen, setSkillsOpen] = useState(false);
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [mcpOpen, setMcpOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);

  const [cwdOpen, setCwdOpen] = useState(false);
  const [cwdDraft, setCwdDraft] = useState("");

  const [authError, setAuthError] = useState<string | null>(null);

  const wsRef = useRef<WebSocket | null>(null);
  const chatEndRef = useRef<HTMLDivElement | null>(null);
  const selectedIdRef = useRef<string | null>(selectedId);

  const sortedSessions = useMemo(() => sortSessions(sessions), [sessions]);

  function handleApiError(err: unknown) {
    if (err instanceof ApiError && err.status === 401) {
      setAuthError("Unauthorized. Re-open the URL printed by `kiliax serve start`.");
      return;
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

  async function fetchSession(sessionId: string) {
    try {
      const s = await api.getSession(sessionId);
      if (selectedIdRef.current !== sessionId) return;
      setSession(s);

      const msgs = await api.getMessages(sessionId, 200);
      if (selectedIdRef.current !== sessionId) return;
      setMessages(msgs.items);
      setPending([]);
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
    if (!selectedId) return;
    const text = composerText.trim();
    if (!text) return;

    try {
      const run: any = await api.createRun(selectedId, {
        input: { type: "text", text },
      });
      setPending((p) => [
        ...p,
        { runId: String(run?.id ?? ""), createdAt: new Date().toISOString(), content: text },
      ]);
      setComposerText("");
      await refreshSessions();
    } catch (err) {
      handleApiError(err);
    }
  }

  async function openSkills() {
    if (!selectedId) return;
    try {
      const res = await api.listSkills(selectedId);
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
    selectedIdRef.current = selectedId;
    if (!selectedId) {
      setSession(null);
      setMessages([]);
      setPending([]);
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
    fetchSession(selectedId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId]);

  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, pending, stream.assistant, stream.thinking, stream.toolCalls.length]);

  const selectedSummary = sortedSessions.find((s) => s.id === selectedId) ?? null;
  const selectedBadge = selectedSummary ? statusBadge(selectedSummary) : null;

  const agentOptions = capabilities?.agents ?? [];
  const modelOptions = capabilities?.models ?? [];

  if (authError) {
    return (
      <div className="h-dvh w-full bg-white text-zinc-900">
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
    <div className="h-dvh w-full bg-white text-zinc-900">
      <div className="flex h-full">
        <aside className="flex w-[280px] flex-col border-r border-zinc-200 bg-zinc-50">
          <div className="p-3">
            <Button className="w-full" onClick={onNewSession}>
              New Session
            </Button>
            <div className="mt-2 grid grid-cols-2 gap-2">
              <Button variant="outline" onClick={openSkills} disabled={!selectedId}>
                Skills
              </Button>
              <Button
                variant="outline"
                onClick={() => setMcpOpen(true)}
                disabled={!selectedId}
              >
                MCP
              </Button>
            </div>
          </div>

          <Separator />

          <div className="flex-1 overflow-auto p-2">
            <div className="px-2 pb-2 text-xs font-medium text-zinc-500">
              Sessions
            </div>
            <div className="space-y-1">
              {sortedSessions.map((s) => {
                const badge = statusBadge(s);
                const active = s.id === selectedId;
                return (
                  <button
                    key={s.id}
                    onClick={() => selectSession(s.id)}
                    className={[
                      "w-full rounded-md px-2 py-2 text-left",
                      active ? "bg-white shadow-sm" : "hover:bg-white/70",
                    ].join(" ")}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <div className="truncate text-sm text-zinc-900">
                        {s.title || s.id}
                      </div>
                      <Badge variant={badge.variant}>{badge.label}</Badge>
                    </div>
                    <div className="mt-1 truncate text-xs text-zinc-500">
                      {s.settings.agent} · {s.settings.model_id}
                    </div>
                  </button>
                );
              })}
              {!sortedSessions.length ? (
                <div className="px-2 py-6 text-center text-sm text-zinc-500">
                  No sessions
                </div>
              ) : null}
            </div>
          </div>

          <Separator />

          <div className="p-3">
            <Button variant="ghost" className="w-full justify-start" onClick={openSettings}>
              Settings
            </Button>
          </div>
        </aside>

        <main className="flex min-w-0 flex-1 flex-col">
          <div className="flex items-center justify-between border-b border-zinc-200 px-4 py-3">
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
                    {session.settings.agent} · {session.settings.model_id}
                  </span>
                ) : null}
              </div>
            </div>
          </div>

          <div className="flex-1 overflow-auto px-4 py-4">
            {selectedId ? (
              <div className="mx-auto w-full max-w-3xl space-y-3">
                {messages.map((m) => (
                  <MessageRow key={`${m.role}:${m.id}`} msg={m} />
                ))}

                {pending.map((p) => (
                  <div key={`pending:${p.runId}`} className="flex justify-end">
                    <div className="max-w-[78%] rounded-2xl bg-zinc-900/90 px-4 py-2 text-sm text-zinc-50">
                      {p.content}
                      <div className="mt-1 text-xs text-zinc-200">queued</div>
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
                        <pre className="mt-2 overflow-auto rounded bg-zinc-50 p-2 text-xs text-zinc-800">
                          {c.arguments}
                        </pre>
                      </details>
                    ))}
                  </div>
                ) : null}

                {stream.assistant ? (
                  <div className="flex justify-start">
                    <div className="max-w-[78%] rounded-2xl border border-zinc-200 bg-white px-4 py-2 text-sm text-zinc-900">
                      <div className="whitespace-pre-wrap">{stream.assistant}</div>
                    </div>
                  </div>
                ) : null}

                <div ref={chatEndRef} />
              </div>
            ) : (
              <EmptyState />
            )}
          </div>

          <div className="border-t border-zinc-200 bg-white px-4 py-3">
            <div className="mx-auto w-full max-w-3xl">
              <div className="mb-2 flex flex-wrap items-center gap-2">
                <label className="text-xs text-zinc-600">Agent</label>
                <select
                  className="h-8 rounded-md border border-zinc-200 bg-white px-2 text-xs"
                  value={session?.settings.agent ?? ""}
                  disabled={!session}
                  onChange={(e) => patchSession({ agent: e.target.value })}
                >
                  <option value="" disabled>
                    -
                  </option>
                  {agentOptions.map((a) => (
                    <option key={a} value={a}>
                      {a}
                    </option>
                  ))}
                </select>

                <label className="ml-2 text-xs text-zinc-600">Model</label>
                <select
                  className="h-8 min-w-[220px] rounded-md border border-zinc-200 bg-white px-2 text-xs"
                  value={session?.settings.model_id ?? ""}
                  disabled={!session}
                  onChange={(e) => patchSession({ model_id: e.target.value })}
                >
                  <option value="" disabled>
                    -
                  </option>
                  {modelOptions.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>

                <Button
                  variant="outline"
                  size="sm"
                  disabled={!session}
                  onClick={() =>
                    patchSession({
                      workspace_root: tmpWorkspacePath(),
                    })
                  }
                >
                  New tmp cwd
                </Button>

                <Button
                  variant="outline"
                  size="sm"
                  disabled={!session}
                  onClick={() => {
                    setCwdDraft(session?.settings.workspace_root ?? "");
                    setCwdOpen(true);
                  }}
                >
                  Set cwd
                </Button>

                {session ? (
                  <div className="truncate text-xs text-zinc-600">
                    cwd:{" "}
                    <span className="font-mono text-zinc-800">
                      {session.settings.workspace_root}
                    </span>
                  </div>
                ) : null}
              </div>

              <div className="flex items-end gap-2">
                <Textarea
                  value={composerText}
                  onChange={(e) => setComposerText(e.target.value)}
                  placeholder="Ask anything…"
                  className="min-h-[52px] resize-none"
                  disabled={!selectedId}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      onSend();
                    }
                  }}
                />
                <Button onClick={onSend} disabled={!selectedId || !composerText.trim()}>
                  Send
                </Button>
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
            <DialogDescription>Discovered for current workspace</DialogDescription>
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
            <DialogDescription>Toggle per-session MCP servers</DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            {(session?.settings.mcp.servers ?? []).map((s) => (
              <label
                key={s.id}
                className="flex items-center justify-between rounded-md border border-zinc-200 bg-white px-3 py-2"
              >
                <div className="min-w-0">
                  <div className="truncate text-sm">{s.id}</div>
                  <div className="mt-0.5 truncate text-xs text-zinc-600">
                    {(session?.mcp_status ?? [])
                      .find((x) => x.id === s.id)
                      ?.state?.toString() ?? "unknown"}
                  </div>
                </div>
                <input
                  type="checkbox"
                  checked={s.enable}
                  onChange={(e) =>
                    patchSession({
                      mcp: { servers: [{ id: s.id, enable: e.target.checked }] },
                    })
                  }
                />
              </label>
            ))}
            {!session?.settings.mcp.servers.length ? (
              <div className="text-center text-sm text-zinc-500">No MCP servers</div>
            ) : null}
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
