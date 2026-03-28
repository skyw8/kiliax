import type {
  Capabilities,
  ConfigResponse,
  ConfigUpdateRequest,
  MessageListResponse,
  RunCreateRequest,
  Session,
  SessionListResponse,
  SkillListResponse,
} from "@/lib/types";

const TOKEN_KEY = "kiliax_token";

export class ApiError extends Error {
  status: number;
  code?: string;

  constructor(status: number, message: string, code?: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

export function getStoredToken(): string {
  try {
    return localStorage.getItem(TOKEN_KEY) ?? "";
  } catch {
    return "";
  }
}

export function setStoredToken(token: string) {
  try {
    localStorage.setItem(TOKEN_KEY, token.trim());
  } catch {
    // ignore
  }
}

async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const token = getStoredToken();
  const headers = new Headers(init?.headers ?? {});
  headers.set("Accept", "application/json");
  if (!headers.has("Content-Type") && init?.body) {
    headers.set("Content-Type", "application/json");
  }
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }

  const resp = await fetch(path, { ...init, headers });
  if (resp.status === 204) {
    return undefined as T;
  }
  const text = await resp.text();
  const json = text ? (JSON.parse(text) as any) : null;

  if (!resp.ok) {
    const code = json?.error?.code ?? undefined;
    const message = json?.error?.message ?? resp.statusText;
    throw new ApiError(resp.status, message, code);
  }
  return json as T;
}

export function wsUrl(path: string): string {
  const proto = window.location.protocol === "https:" ? "wss" : "ws";
  const token = getStoredToken();
  const url = new URL(path, `${proto}://${window.location.host}`);
  if (token) {
    url.searchParams.set("token", token);
  }
  return url.toString();
}

export const api = {
  getCapabilities(): Promise<Capabilities> {
    return apiFetch<Capabilities>("/v1/capabilities");
  },
  listSessions(): Promise<SessionListResponse> {
    return apiFetch<SessionListResponse>("/v1/sessions");
  },
  createSession(): Promise<Session> {
    return apiFetch<Session>("/v1/sessions", { method: "POST", body: "{}" });
  },
  getSession(sessionId: string): Promise<Session> {
    return apiFetch<Session>(`/v1/sessions/${sessionId}`);
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
  listSkills(sessionId: string): Promise<SkillListResponse> {
    return apiFetch<SkillListResponse>(`/v1/sessions/${sessionId}/skills`);
  },
};

