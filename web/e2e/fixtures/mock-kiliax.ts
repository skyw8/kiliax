import type { Page, Route } from "@playwright/test";

type HttpMethod = "DELETE" | "GET" | "PATCH" | "POST" | "PUT";

export type MockRequest = {
  method: HttpMethod;
  path: string;
  body: any;
  authorization: string | null;
};

type SessionSettings = {
  agent: string;
  model_id: string;
  workspace_root: string;
  extra_workspace_roots: string[];
  skills: { default_enable: boolean; overrides: any[] };
  custom_tools: { default_enable: boolean; overrides: any[] };
  mcp: { servers: any[] };
};

type SessionSummary = {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  last_outcome: "none" | "done" | "error";
  status: {
    run_state: "idle" | "running" | "tooling" | "retrying";
    active_run_id: string | null;
    active_run_started_at?: string | null;
    step: number;
    active_tool?: string | null;
    retry_status?: any;
    queue_len: number;
    last_event_id: number;
  };
  settings: SessionSettings;
  goal?: any;
};

type Provider = {
  id: string;
  api: string;
  base_url: string;
  api_key_set: boolean;
  models: Array<
    | string
    | {
        id: string;
        auto_compact_token_limit?: number | null;
        temperature?: number | null;
        reasoning_effort?: string | null;
      }
  >;
};

type MockOptions = {
  unauthorized?: boolean;
};

const now = "2026-05-30T09:00:00.000Z";

function settings(root: string): SessionSettings {
  return {
    agent: "master",
    model_id: "openai/gpt-4.1-mini",
    workspace_root: root,
    extra_workspace_roots: [],
    skills: { default_enable: true, overrides: [] },
    custom_tools: { default_enable: true, overrides: [] },
    mcp: { servers: [] },
  };
}

function summary(id: string, title: string, root: string, step = 1): SessionSummary {
  return {
    id,
    title,
    created_at: now,
    updated_at: now,
    last_outcome: "done",
    status: {
      run_state: "idle",
      active_run_id: null,
      active_run_started_at: null,
      step,
      active_tool: null,
      retry_status: null,
      queue_len: 0,
      last_event_id: step,
    },
    settings: settings(root),
    goal: null,
  };
}

function toSession(item: SessionSummary) {
  return {
    ...item,
    mcp_status: [
      {
        id: "filesystem",
        enable: item.settings.mcp.servers.find((s: any) => s.id === "filesystem")?.enable ?? true,
        state: "connected",
        last_error: null,
        tools: ["read_file"],
      },
    ],
    stream: null,
  };
}

async function body(route: Route) {
  const raw = route.request().postData();
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

export async function installMockKiliax(page: Page, options: MockOptions = {}) {
  if (!options.unauthorized) {
    await page.addInitScript(() => sessionStorage.setItem("kiliax_token", "secret-token"));
  }
  const sessions = new Map<string, SessionSummary>([
    ["s-work", summary("s-work", "Workspace thread", "D:\\github_code\\kiliax", 12)],
    ["s-tmp", summary("s-tmp", "Scratch thread", "D:\\github_code\\kiliax\\.kiliax\\workspace\\tmp_s-tmp", 1)],
  ]);
  const messages = new Map<string, any[]>([
    [
      "s-work",
      [
        {
          role: "user",
          id: "1",
          created_at: now,
          content: "Summarize the repository.",
        },
        {
          role: "assistant",
          id: "2",
          created_at: now,
          content: "This repository contains a Rust control plane and a React Web UI.",
          usage: { prompt_tokens: 11, completion_tokens: 9, total_tokens: 20 },
        },
      ],
    ],
    ["s-tmp", []],
  ]);
  const providers: Provider[] = [
    {
      id: "openai",
      api: "openai_chat_completions",
      base_url: "https://api.openai.com/v1",
      api_key_set: true,
      models: [{ id: "gpt-4.1-mini", auto_compact_token_limit: 4096 }],
    },
  ];
  const requests: MockRequest[] = [];
  const skills = [
    { id: "imagegen", name: "imagegen", description: "Generate images" },
    { id: "github", name: "github", description: "GitHub workflow helpers" },
  ];
  const customTools = [
    { id: "lint_project", name: "lint_project", description: "Run project lint", parallel: false },
  ];
  let defaultModel = "openai/gpt-4.1-mini";
  let nextSession = 10;
  let nextRun = 20;
  let runtime = {
    runtime_max_steps: 64,
    agents_plan_max_steps: 12,
    agents_general_max_steps: 24,
  };

  await page.addInitScript(() => {
    class MockWebSocket {
      static CONNECTING = 0;
      static OPEN = 1;
      static CLOSING = 2;
      static CLOSED = 3;

      url: string;
      readyState = MockWebSocket.CONNECTING;
      onopen: ((event: Event) => void) | null = null;
      onmessage: ((event: MessageEvent) => void) | null = null;
      onerror: ((event: Event) => void) | null = null;
      onclose: ((event: CloseEvent) => void) | null = null;

      constructor(url: string) {
        this.url = url;
        (window as any).__kiliaxSockets = (window as any).__kiliaxSockets ?? [];
        (window as any).__kiliaxSockets.push(this);
        window.setTimeout(() => {
          this.readyState = MockWebSocket.OPEN;
          this.onopen?.(new Event("open"));
        }, 0);
      }

      send(data: string) {
        (window as any).__kiliaxWsMessages = (window as any).__kiliaxWsMessages ?? [];
        (window as any).__kiliaxWsMessages.push(data);
      }

      close() {
        this.readyState = MockWebSocket.CLOSED;
        this.onclose?.(new CloseEvent("close"));
      }
    }

    (window as any).WebSocket = MockWebSocket as any;
    (window as any).__kiliaxEmitWs = (event: any) => {
      const data = JSON.stringify(event);
      for (const socket of (window as any).__kiliaxSockets ?? []) {
        if (socket.readyState === MockWebSocket.OPEN) {
          socket.onmessage?.(new MessageEvent("message", { data }));
        }
      }
    };
  });

  function record(method: HttpMethod, path: string, reqBody: any, authorization: string | null) {
    requests.push({ method, path, body: reqBody, authorization });
  }

  async function fulfill(route: Route, value: any, status = 200) {
    await route.fulfill({
      status,
      contentType: "application/json",
      body: value === undefined ? "" : JSON.stringify(value),
    });
  }

  await page.route("**/v1/**", async (route) => {
    const req = route.request();
    const url = new URL(req.url());
    const method = req.method().toUpperCase() as HttpMethod;
    const path = url.pathname;
    const reqBody = await body(route);
    record(method, path, reqBody, req.headers()["authorization"] ?? null);

    if (options.unauthorized && path !== "/v1/capabilities") {
      return fulfill(
        route,
        { error: { code: "unauthorized", message: "Unauthorized" } },
        401,
      );
    }

    if (method === "GET" && path === "/v1/capabilities") {
      return fulfill(route, {
        agents: ["master", "explore"],
        models: ["openai/gpt-4.1-mini", "anthropic/claude-3-5-sonnet-latest"],
        builtin_tools: [
          { id: "read_file", name: "read_file", description: "Read a file" },
          { id: "shell_command", name: "shell_command", description: "Run a command" },
        ],
        mcp_servers: [
          {
            id: "filesystem",
            enable: true,
            state: "connected",
            last_error: null,
            tools: ["read_file"],
          },
        ],
      });
    }

    if (method === "GET" && path === "/v1/sessions") {
      return fulfill(route, {
        items: Array.from(sessions.values()).sort((a, b) => b.updated_at.localeCompare(a.updated_at)),
        next_cursor: null,
      });
    }

    if (method === "POST" && path === "/v1/sessions") {
      const id = `s-new-${nextSession++}`;
      const root = reqBody?.settings?.workspace_root ?? `D:\\tmp\\kiliax-session-${id}`;
      const item = summary(id, reqBody?.title ?? "New thread", root, 0);
      if (reqBody?.settings?.agent) item.settings.agent = reqBody.settings.agent;
      if (reqBody?.settings?.model_id) item.settings.model_id = reqBody.settings.model_id;
      sessions.set(id, item);
      messages.set(id, []);
      return fulfill(route, toSession(item));
    }

    const sessionMatch = path.match(/^\/v1\/sessions\/([^/]+)$/);
    if (sessionMatch && method === "GET") {
      const item = sessions.get(decodeURIComponent(sessionMatch[1]));
      return item ? fulfill(route, toSession(item)) : fulfill(route, { error: "not found" }, 404);
    }

    if (sessionMatch && method === "DELETE") {
      const id = decodeURIComponent(sessionMatch[1]);
      sessions.delete(id);
      messages.delete(id);
      return fulfill(route, undefined, 204);
    }

    const messagesMatch = path.match(/^\/v1\/sessions\/([^/]+)\/messages$/);
    if (messagesMatch && method === "GET") {
      const id = decodeURIComponent(messagesMatch[1]);
      return fulfill(route, { items: messages.get(id) ?? [], next_before: null });
    }

    const runMatch = path.match(/^\/v1\/sessions\/([^/]+)\/runs$/);
    if (runMatch && method === "POST") {
      return fulfill(route, { id: `run-${nextRun++}` });
    }

    const cancelMatch = path.match(/^\/v1\/runs\/([^/]+)\/cancel$/);
    if (cancelMatch && method === "POST") {
      return fulfill(route, { id: decodeURIComponent(cancelMatch[1]), cancelled: true });
    }

    const goalMatch = path.match(/^\/v1\/sessions\/([^/]+)\/goal$/);
    if (goalMatch && method === "PUT") {
      const id = decodeURIComponent(goalMatch[1]);
      const item = sessions.get(id)!;
      item.goal = {
        objective: reqBody.objective,
        status: "active",
        session_id: id,
        time_used_seconds: 0,
        created_at: now,
        updated_at: now,
        tokens_used: 0,
      };
      return fulfill(route, item.goal);
    }
    if (goalMatch && method === "DELETE") {
      const item = sessions.get(decodeURIComponent(goalMatch[1]));
      if (item) item.goal = null;
      return fulfill(route, undefined, 204);
    }

    const settingsMatch = path.match(/^\/v1\/sessions\/([^/]+)\/settings$/);
    if (settingsMatch && method === "PATCH") {
      const item = sessions.get(decodeURIComponent(settingsMatch[1]))!;
      item.settings = {
        ...item.settings,
        ...reqBody,
        skills: { ...item.settings.skills, ...(reqBody.skills ?? {}) },
        custom_tools: { ...item.settings.custom_tools, ...(reqBody.custom_tools ?? {}) },
        mcp: { ...item.settings.mcp, ...(reqBody.mcp ?? {}) },
      };
      return fulfill(route, toSession(item));
    }

    if (
      (method === "GET" && path === "/v1/skills") ||
      (method === "GET" && path.match(/^\/v1\/sessions\/[^/]+\/skills$/))
    ) {
      return fulfill(route, { items: skills, errors: [] });
    }

    if (
      (method === "GET" && path === "/v1/custom-tools") ||
      (method === "GET" && path.match(/^\/v1\/sessions\/[^/]+\/custom-tools$/))
    ) {
      return fulfill(route, { items: customTools, errors: [] });
    }

    if (path.endsWith("/settings/save-defaults") && method === "POST") {
      return fulfill(route, undefined, 204);
    }

    if (path.endsWith("/open") && method === "POST") {
      return fulfill(route, undefined, 204);
    }

    if (path.endsWith("/fork") && method === "POST") {
      const id = `s-fork-${nextSession++}`;
      const item = summary(id, "Forked thread", "D:\\github_code\\kiliax", 0);
      sessions.set(id, item);
      messages.set(id, []);
      return fulfill(route, { session: toSession(item) });
    }

    if (method === "GET" && path === "/v1/fs/list") {
      const requested = url.searchParams.get("path") ?? "D:\\";
      return fulfill(route, {
        path: requested,
        parent: requested === "D:\\" ? null : "D:\\",
        entries: [
          { name: "github_code", path: "D:\\github_code", is_dir: true },
          { name: "kiliax", path: "D:\\github_code\\kiliax", is_dir: true },
          { name: "fixtures", path: "D:\\fixtures", is_dir: true },
        ],
      });
    }

    if (method === "GET" && path === "/v1/config/providers") {
      return fulfill(route, { default_model: defaultModel, providers });
    }

    if (method === "PATCH" && path === "/v1/config/providers") {
      if ("default_model" in reqBody) defaultModel = reqBody.default_model ?? "";
      for (const deleted of reqBody.delete ?? []) {
        const idx = providers.findIndex((p) => p.id === deleted);
        if (idx >= 0) providers.splice(idx, 1);
      }
      for (const upsert of reqBody.upsert ?? []) {
        const existing = providers.find((p) => p.id === upsert.id);
        if (existing) {
          Object.assign(existing, {
            api: upsert.api ?? existing.api,
            base_url: upsert.base_url ?? existing.base_url,
            models: upsert.models ?? existing.models,
            api_key_set:
              upsert.api_key === null
                ? false
                : upsert.api_key
                  ? true
                  : existing.api_key_set,
          });
        } else {
          providers.push({
            id: upsert.id,
            api: upsert.api ?? "openai_chat_completions",
            base_url: upsert.base_url ?? "",
            api_key_set: Boolean(upsert.api_key),
            models: upsert.models ?? [],
          });
        }
      }
      return fulfill(route, undefined, 204);
    }

    if (method === "GET" && path === "/v1/config/runtime") {
      return fulfill(route, runtime);
    }

    if (method === "PATCH" && path === "/v1/config/runtime") {
      runtime = { ...runtime, ...reqBody };
      return fulfill(route, undefined, 204);
    }

    if (method === "GET" && path === "/v1/config") {
      return fulfill(route, { path: "D:\\Users\\test\\.kiliax\\kiliax.yaml", yaml: "default_model: openai/gpt-4.1-mini\n", config: {} });
    }

    if (method === "PUT" && path === "/v1/config") {
      return fulfill(route, { path: "D:\\Users\\test\\.kiliax\\kiliax.yaml", yaml: reqBody.yaml, config: {} });
    }

    return fulfill(route, { error: `Unhandled ${method} ${path}` }, 500);
  });

  return {
    sessions,
    messages,
    providers,
    requests,
    emitWs: async (event: any) => page.evaluate((ev) => (window as any).__kiliaxEmitWs(ev), event),
  };
}
