## description
a strong, high performance, cross-platform AI Agent tool
subagents
token efficient
minimal

## philosophy

- high cohesion, low coupling
- keep it simple, stupid
- less is more

## tech stack

- rust
- [rust-sdk](https://github.com/modelcontextprotocol/rust-sdk) MCP
- [async-openai](https://github.com/64bit/async-openai) openai compatible API

- react
- shadcn
- bun
- vite

## arch

### crates/kiliax-core (core library)

- Agents + explicit subagent availability + tool permissions + built-in master/explore + goal/multi-agent toolsets: `crates/kiliax-core/src/agents/`
- Context compaction (auto + `/compact`, tool call/result normalization + tool output truncation): `crates/kiliax-core/src/compact.rs` (prompts: `crates/kiliax-core/prompts/compact/`)
- Config + model/agent defaults + provider-model max-output/auto-compact/temperature/reasoning-effort limits + multi-agent limits + invalid default_model fallback + provider API routing: `crates/kiliax-core/src/config.rs`
- Shared path validation (tilde/absolute/dir): `crates/kiliax-core/src/paths.rs`
- Protocol compatibility re-exports (messages/tool-calls/usage + base64 image/PDF content parts): `crates/kiliax-core/src/protocol.rs`
- MCP enablement overrides (shared semantics): `crates/kiliax-core/src/mcp_overrides.rs`
- LLM compatibility re-exports + core telemetry hook: `crates/kiliax-core/src/llm.rs`
- Provider-neutral message history sanitization + request-safety helpers: `crates/kiliax-core/src/history.rs`
- Built-in and auto-discovered custom agent profiles (global `~/.kiliax/agents/*/AGENT.yaml` + `PROMPT.md`): `crates/kiliax-core/src/agents/`
- Prompt assembly + single-system preamble for provider compatibility + nested project instruction scoping + multi-agent capability hint + available subagent descriptions + project prompts last in preamble (stable during normal turns, refreshed after compaction): `crates/kiliax-core/src/prompt.rs`
- Agent runtime loop + LLM retry/backoff event emission + tool scheduling barriers + thinking/body normalization + empty assistant guard: `crates/kiliax-core/src/runtime.rs`
- Streaming step assembly (thinking/body/tool calls): `crates/kiliax-core/src/runtime/streaming.rs`
- Session store + snapshots + append-only events + reverse paged visible-message reads + frozen project prompt metadata + multi-agent parent/path metadata + session-scoped MCP/skills/custom-tools overrides + persistent session goal state/accounting: `crates/kiliax-core/src/session.rs`
- Telemetry capture + span attributes/naming + metrics: `crates/kiliax-core/src/telemetry.rs`
- Tools (builtin registry/patch application/MCP dispatch + Cargo-version client identity/skills discovery/custom tool discovery from `~/.kiliax/tools` + JSON-RPC process runtime/goal toolset backend dispatch + multi-agent backend dispatch + tool telemetry categories/outcomes/failed-call output capture): `crates/kiliax-core/src/tools/`
- Builtin tools (`crates/kiliax-core/src/tools/builtin/`):
  - `read_file`: read a line-numbered UTF-8 text file from the workspace (or allowed skills roots) using `filePath`, `offset`, and `limit`
  - `list_dir`: list directory entries under the workspace, optional recursive/depth/hidden/limit
  - `grep_files`: search files for a regex pattern (ripgrep semantics; respects `.gitignore`/`.ignore` by default)
  - `view_image`: attach a local image from the filesystem (png/jpg/jpeg/gif/webp/bmp/tif/tiff/avif)
  - `shell_command`: run a command string in the workspace through the user's default or requested shell, inheriting the full process environment and using login/profile semantics by default; supports timeout, long-running `session_id` polling, bounded/truncated output, Unix PTY via `tty=true`, and Codex/opencode argument aliases (`command`/`workdir`/`timeout`/`description`)
  - `write_stdin`: write to stdin of a running shell session, or poll its output; reports timeout/truncation/status metadata consistently with `shell_command`
  - `write_file`: write/overwrite a UTF-8 text file using `filePath` and `content`, creating parent directories and preserving UTF-8 BOM
  - `edit_file`: perform opencode-style text edits using `filePath`, `oldString`, `newString`, and `replaceAll`, preserving BOM/line endings and allowing empty `oldString` for whole-file create/overwrite
  - `apply_patch`: apply a stripped-down file-oriented diff envelope (`*** Begin Patch` / `*** End Patch`) for multi-file edits
  - `update_plan`: update the UI plan (best effort, surfaced in web)
  - `get_goal`: read the active session goal (`SessionGoal`) if present
  - `update_goal`: mark the active session goal complete (`status=complete` only)
  - `spawn_agent` / `send_message` / `followup_task` / `wait_agent` / `list_agents` / `close_agent`: master-facing multi-agent orchestration tools backed by the server runtime
  - `web_search`: search the web via Tavily (`web_search.api_key` / `tools.tavily.api_key` in `kiliax.yaml`, fallback `TAVILY_API_KEY`)

### crates/kiliax-llm (LLM facade + providers)

- Provider-neutral LLM facade, provider API routing, shared LLM error classification/retry policy, and telemetry hook interface: `crates/kiliax-llm/src/lib.rs`, `crates/kiliax-llm/src/telemetry.rs`
- Protocol types (messages/tool-calls/usage/stream chunks + image/PDF user content parts) + provider-safe tool-name aliasing: `crates/kiliax-llm/src/types.rs`, `crates/kiliax-llm/src/tool_names.rs`
- OpenAI-compatible Chat Completions client + BYOT compatibility (streaming/tool-calls/usage + base64 image/PDF request parts + thinking-provider `reasoning_content` compatibility + per-model temperature/reasoning_effort + Langfuse completion timing): `crates/kiliax-llm/src/openai_*.rs`, `crates/kiliax-llm/src/byot.rs`, `crates/kiliax-llm/src/patches.rs`
- OpenAI Responses API provider (request conversion, base64 image/PDF input parts, SSE events, prompt cache key forwarding, DashScope session-cache header + usage fallback, function-call/reasoning item replay + function-tool aliasing + Langfuse wire-request generation input/output/usage capture): `crates/kiliax-llm/src/openai_responses.rs`
- Anthropic Messages API provider (non-streaming + SSE/tool-use mapping + Claude/configured model max-token resolution + effort/adaptive thinking config + thinking block preservation/replay + base64 image/PDF blocks + grouped tool_result request blocks + parallel tool-use controls + Langfuse wire-request generation input/output/usage capture): `crates/kiliax-llm/src/anthropic.rs`

### crates/kiliax-cli (CLI)

- CLI command routing + installed `ki` entrypoint (source package remains `kiliax`) => ensure server is running and open Web UI (silent first-run config init) + local/remote MCP export over stdio or Streamable HTTP (`ki mcp serve [--transport stdio|http]`) + local session goal commands: `crates/kiliax-cli/src/main.rs`
- Server daemon control + idempotent start via bearer API/admin identity checks: `crates/kiliax-cli/src/daemon.rs`
- Foreground server run argument parsing: `crates/kiliax-cli/src/server_run_args.rs`

### crates/kiliax-mcp (MCP export adapter)

- MCP server adapter that exposes kiliax as an agent service for other agents, forwarding tools/resources/prompts/completions to the running HTTP control plane over stdio and returning structured tool results: `crates/kiliax-mcp/src/lib.rs`
- Streamable HTTP MCP transport (`/mcp` by default) with bearer auth, Origin validation, POST JSON-RPC handling, and GET/DELETE 405 fallback for clients that probe server-initiated SSE/session control: `crates/kiliax-mcp/src/http_transport.rs`
- MCP schema definitions for tools/resources/resource templates/prompts covering capabilities, agent/session listing, session snapshots/messages, run creation/continuation/cancellation, single-skill invocation, and skills enablement: `crates/kiliax-mcp/src/protocol.rs`

### crates/kiliax-server (HTTP control plane)

- Runner (`ki server run` when installed): `crates/kiliax-server/src/runner.rs`
- HTTP router/handlers/auth/local access logs/WS/SSE/OpenAPI/web asset selection/server-side folder listing/session actions/session goal APIs + JSON body limits for base64 attachments: `crates/kiliax-server/src/http/`
- HTTP <-> state domain mappers + client-safe path display normalization: `crates/kiliax-server/src/http/mapper.rs`
- State (config/session lifecycle/run queue with per-run model/agent/MCP/skills/custom-tools overrides + goal continuation loop/retrying status + multi-agent registry and mailbox/goal usage events with output-token accounting/durable-vs-ephemeral events including persisted `user_message` acks + paged message history API + live stream snapshots with settled tool-call pruning/tmp workspace cleanup/default persistence): `crates/kiliax-server/src/state/`
- Multi-agent control plane (root-scoped agent registry, task paths, mailbox updates, close semantics, tool backend): `crates/kiliax-server/src/state/multi_agent.rs`
- Live session runtime integration (spawned child sessions, tool backend wiring, forked context, mailbox delivery, parent notifications, persisted user-message event emission): `crates/kiliax-server/src/state/live_session.rs`
- State domain types (events/status including active run start/snapshots/live stream snapshots/runs/messages/session goals + attachment metadata/image preview data/base64 run input/client message ids): `crates/kiliax-server/src/state/domain.rs`
- Infra (path validation/tmp workspace helpers/client path display normalization/workspace hooks/external launchers + terminal cwd normalization): `crates/kiliax-server/src/infra.rs`
- REST/OpenAPI schemas (includes capabilities builtin tool summaries, message `usage`, live stream snapshots, server-side folder listing, session default writes, run client message ids, run overrides, and run/message attachments with image preview data): `crates/kiliax-server/src/api.rs`
- OpenAPI metadata: `crates/kiliax-server/src/openapi.rs`

### crates/kiliax-otel (OpenTelemetry)

- OTLP exporters/providers + tracing/metrics/logs wiring: `crates/kiliax-otel/src/lib.rs`

### web (React UI)

- Main UI (responsive layout + virtualized/paged session history + centered composer dock/attached workspace launchers + WS streaming with buffered live snapshot restore/active tool-call reconciliation/persisted user-message reconciliation/retry alerts/session actions/goal controls with live time/token updates + workspace folders list + tools catalog entry + server-side folder picker dialogs + composer image/PDF attachment selection, preview, and base64 run submission): `web/src/app.tsx`
- Message rendering + user input collapse controls + queued user bubble styling + user attachment previews/chips + consistent thinking/tool call/result panel sizing: `web/src/components/message-row.tsx`
- Virtualized chat list + paged history windowing + pinned-bottom follow state for streaming row height changes: `web/src/components/virtualized-list.tsx`
- Dialog components including server-side folder picker path normalization and provider/model settings for per-model compact/temperature/reasoning controls: `web/src/components/*-dialog.tsx`, `web/src/components/folder-picker.tsx`
- Action sheet/menu components: `web/src/components/*-actions.tsx`
- UI primitives (performant Dialog/Sheet overlays + Button/Input/etc): `web/src/components/ui/`
- Build + dev server (Vite config/proxy): `web/vite.config.ts`
- Web UI E2E coverage (Playwright desktop/mobile projects + mocked HTTP/WS backend for session, run streaming/retry/error/cancel, session actions, history edit/regenerate, attachments, goal, folder picker, skills/tools/MCP, auth, and settings flows): `web/playwright.config.ts`, `web/e2e/`
- API client + typed provider-model config payloads + server-side folder listing + explicit session default persistence + goal APIs + display/path formatters: `web/src/lib/api.ts`, `web/src/lib/types.ts`, `web/src/lib/app-utils.ts`, `web/src/lib/workspace-utils.ts`
- Alert/toast state: `web/src/lib/use-alerts.ts`
- Types (includes message `usage`, retry status, live stream snapshots, session default writes, run client message ids, and base64 run attachments): `web/src/lib/types.ts`

## constraints
**All UI languages default to English, including any prompts, outputs, etc.**

### Prompt Cache

- Keep stable prompt parts before volatile content to improve provider prompt-cache reuse.
- Place project instructions (`AGENTS.md`/`CLAUDE.md`) last in the system preamble.
- Reuse the session project prompt during normal turns; refresh it only after successful `/compact` or auto-compaction.

### API

- Symptom: `HTTP 400 ... thinking is enabled but reasoning_content is missing in assistant tool call message at index N` or `The reasoning_content in the thinking mode must be passed back to the API`.
- Why: Some OpenAI-compatible thinking APIs validate the full `messages[]` history; when thinking is enabled, every `assistant` message that contains `tool_calls` must include a non-empty `reasoning_content`.
- Rule: always send `reasoning_content` for `assistant` tool-call messages; if there is no reasoning text, send a single whitespace (`" "`) (some gateways treat `""` as missing).
- Implementation: patch outbound JSON in `crates/kiliax-llm/src/patches.rs` (`inject_reasoning_content_for_tool_calls(...)`) and retry once when the provider returns this specific error.
- Provider routing: enable the compatibility field by default for OpenAI Chat Completions-compatible providers, but skip official OpenAI Chat Completions because `reasoning_content` is not part of the standard OpenAI message schema (see `should_inject_reasoning_content(...)`).

### Web

- The session status badge (e.g. `step 1`) in the left sidebar must display in a single line and must not wrap to two lines

### Remote Connection

- All features must be designed with remote server usage in mind; the web UI may be running against a remote `kiliax server` rather than localhost
- Example: file/folder picking must use a server-side web picker (not a native file picker), because native pickers can only access the browser's local filesystem, not the remote server's filesystem

## ATTENTION

- After modifying code, update the arch section in AGENTS.md, only describing these core code parts
- **Do not maintain compatibility with old interfaces or old code—delete and refactor directly**
