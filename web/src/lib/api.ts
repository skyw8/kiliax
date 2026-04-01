import type {
  Capabilities,
  ConfigResponse,
  ConfigProvidersPatchRequest,
  ConfigProvidersResponse,
  ConfigRuntimePatchRequest,
  ConfigRuntimeResponse,
  ConfigSkillsResponse,
  ConfigUpdateRequest,
  FsListResponse,
  MessageListResponse,
  McpServerSetting,
  OpenWorkspaceTarget,
  RunCreateRequest,
  Session,
  SessionListResponse,
  SkillEnableSetting,
  SkillListResponse,
} from "@/lib/types";

export class ApiError extends Error {
  status: number;
  code?: string;
  traceId?: string;
  details?: unknown;
  bodyText?: string;

  constructor(status: number, message: string, code?: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

function safeJsonParse(text: string): any | null {
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers ?? {});
  headers.set("Accept", "application/json");
  if (!headers.has("Content-Type") && init?.body) {
    headers.set("Content-Type", "application/json");
  }

  const resp = await fetch(path, { ...init, headers });
  if (resp.status === 204) {
    return undefined as T;
  }
  const text = await resp.text();
  const json = safeJsonParse(text);

  if (!resp.ok) {
    const code = json?.error?.code ?? undefined;
    const message = json?.error?.message ?? (text || resp.statusText);
    const err = new ApiError(resp.status, message, code);
    err.traceId = json?.trace_id ?? undefined;
    err.details = json?.error?.details ?? undefined;
    err.bodyText = text || undefined;
    throw err;
  }
  if (json == null) {
    const err = new ApiError(resp.status, "Invalid JSON response");
    err.bodyText = text || undefined;
    throw err;
  }
  return json as T;
}

export function wsUrl(path: string): string {
  const proto = window.location.protocol === "https:" ? "wss" : "ws";
  const url = new URL(path, `${proto}://${window.location.host}`);
  return url.toString();
}

export const api = {
  getCapabilities(): Promise<Capabilities> {
    return apiFetch<Capabilities>("/v1/capabilities");
  },
  listSessions(): Promise<SessionListResponse> {
    return apiFetch<SessionListResponse>("/v1/sessions");
  },
  createSession(req?: { title?: string; settings?: { workspace_root?: string } }): Promise<Session> {
    return apiFetch<Session>("/v1/sessions", {
      method: "POST",
      body: JSON.stringify(req ?? {}),
    });
  },
  getSession(sessionId: string): Promise<Session> {
    return apiFetch<Session>(`/v1/sessions/${sessionId}`);
  },
  deleteSession(sessionId: string): Promise<void> {
    return apiFetch<void>(`/v1/sessions/${sessionId}`, { method: "DELETE" });
  },
  patchSessionSettings(sessionId: string, patch: unknown): Promise<Session> {
    return apiFetch<Session>(`/v1/sessions/${sessionId}/settings`, {
      method: "PATCH",
      body: JSON.stringify(patch),
    });
  },
  getMessages(sessionId: string, limit = 200): Promise<MessageListResponse> {
    return apiFetch<MessageListResponse>(
      `/v1/sessions/${sessionId}/messages?limit=${limit}`,
    );
  },
  createRun(sessionId: string, req: RunCreateRequest) {
    return apiFetch(`/v1/sessions/${sessionId}/runs`, {
      method: "POST",
      body: JSON.stringify({ auto_resume: true, ...req }),
    });
  },
  cancelRun(runId: string) {
    return apiFetch(`/v1/runs/${runId}/cancel`, { method: "POST" });
  },
  getConfig(): Promise<ConfigResponse> {
    return apiFetch<ConfigResponse>("/v1/config");
  },
  putConfig(req: ConfigUpdateRequest): Promise<ConfigResponse> {
    return apiFetch<ConfigResponse>("/v1/config", {
      method: "PUT",
      body: JSON.stringify(req),
    });
  },
  getConfigProviders(): Promise<ConfigProvidersResponse> {
    return apiFetch<ConfigProvidersResponse>("/v1/config/providers");
  },
  patchConfigProviders(req: ConfigProvidersPatchRequest): Promise<void> {
    return apiFetch<void>("/v1/config/providers", {
      method: "PATCH",
      body: JSON.stringify(req),
    });
  },
  getConfigRuntime(): Promise<ConfigRuntimeResponse> {
    return apiFetch<ConfigRuntimeResponse>("/v1/config/runtime");
  },
  patchConfigRuntime(req: ConfigRuntimePatchRequest): Promise<void> {
    return apiFetch<void>("/v1/config/runtime", {
      method: "PATCH",
      body: JSON.stringify(req),
    });
  },
  patchConfigMcp(req: { servers: McpServerSetting[] }): Promise<void> {
    return apiFetch<void>("/v1/config/mcp", {
      method: "PATCH",
      body: JSON.stringify(req),
    });
  },
  getConfigSkills(): Promise<ConfigSkillsResponse> {
    return apiFetch<ConfigSkillsResponse>("/v1/config/skills");
  },
  patchConfigSkills(req: { default_enable?: boolean; skills: SkillEnableSetting[] }): Promise<void> {
    return apiFetch<void>("/v1/config/skills", {
      method: "PATCH",
      body: JSON.stringify(req),
    });
  },
  listSkills(sessionId: string): Promise<SkillListResponse> {
    return apiFetch<SkillListResponse>(`/v1/sessions/${sessionId}/skills`);
  },
  listGlobalSkills(): Promise<SkillListResponse> {
    return apiFetch<SkillListResponse>("/v1/skills");
  },
  forkSession(sessionId: string, assistantMessageId: string): Promise<any> {
    return apiFetch(`/v1/sessions/${sessionId}/fork`, {
      method: "POST",
      body: JSON.stringify({ assistant_message_id: assistantMessageId }),
    });
  },
  editUserMessage(sessionId: string, userMessageId: string, content: string): Promise<any> {
    return apiFetch(`/v1/sessions/${sessionId}/messages/${userMessageId}/edit`, {
      method: "POST",
      body: JSON.stringify({ content }),
    });
  },
  regenerateAssistantMessage(sessionId: string, assistantMessageId: string): Promise<any> {
    return apiFetch(`/v1/sessions/${sessionId}/messages/${assistantMessageId}/regenerate`, {
      method: "POST",
    });
  },
  fsList(path?: string): Promise<FsListResponse> {
    const qs = path ? `?path=${encodeURIComponent(path)}` : "";
    return apiFetch<FsListResponse>(`/v1/fs/list${qs}`);
  },
  openWorkspace(sessionId: string, target: OpenWorkspaceTarget): Promise<void> {
    return apiFetch<void>(`/v1/sessions/${sessionId}/open`, {
      method: "POST",
      body: JSON.stringify({ target }),
    });
  },
};
