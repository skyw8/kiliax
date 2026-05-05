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
- [ratatui](https://github.com/ratatui/ratatui) TUI

- react
- shadcn
- bun
- vite

## arch

### crates/kiliax-core (core library)

- Agents + tool permissions: `crates/kiliax-core/src/agents/`
- Context compaction (auto + `/compact`): `crates/kiliax-core/src/compact.rs` (prompts: `crates/kiliax-core/prompts/compact/`)
- Config + model/agent defaults + invalid default_model fallback + provider API routing: `crates/kiliax-core/src/config.rs`
- Shared path validation (tilde/absolute/dir): `crates/kiliax-core/src/paths.rs`
- Protocol compatibility re-exports (messages/tool-calls/usage): `crates/kiliax-core/src/protocol.rs`
- MCP enablement overrides (shared semantics): `crates/kiliax-core/src/mcp_overrides.rs`
- LLM compatibility re-exports + core telemetry hook: `crates/kiliax-core/src/llm.rs`
- Prompt assembly + nested project instruction scoping: `crates/kiliax-core/src/prompt.rs`
- Agent runtime loop + tool scheduling barriers + thinking/body normalization: `crates/kiliax-core/src/runtime.rs`
- Streaming step assembly (thinking/body/tool calls): `crates/kiliax-core/src/runtime/streaming.rs`
- Session store + snapshots + events + session-scoped MCP/skills overrides: `crates/kiliax-core/src/session.rs`
- Telemetry capture + span attributes/naming + metrics: `crates/kiliax-core/src/telemetry.rs`
- Tools (builtin patch application/MCP dispatch/skills discovery): `crates/kiliax-core/src/tools/`
- Builtin tools (`crates/kiliax-core/src/tools/builtin/`):
  - `read_file`: read a UTF-8 text file from the workspace (or allowed skills roots), with optional line range and byte cap
  - `list_dir`: list directory entries under the workspace, optional recursive/depth/hidden/limit
  - `grep_files`: search files for a regex pattern (ripgrep semantics; respects `.gitignore`/`.ignore` by default)
  - `view_image`: attach a local image from the filesystem (png/jpg/jpeg/gif/webp/bmp/tif/tiff/avif)
  - `shell_command`: run a command string in the workspace through the user's default shell, inheriting the full process environment and using login/profile semantics by default; returns a `session_id` for long-running processes
  - `write_stdin`: write to stdin of a running shell session, or poll its output
  - `write_file`: write/overwrite a file on the local filesystem (requires prior `read_file` when overwriting)
  - `edit_file`: perform exact string replacements in a file (requires prior `read_file`; supports `replaceAll`)
  - `apply_patch`: apply a stripped-down file-oriented diff envelope (`*** Begin Patch` / `*** End Patch`) for multi-file edits
  - `update_plan`: update the UI plan (best effort, surfaced in TUI/web)
  - `web_search`: search the web via Tavily (`web_search.api_key` / `tools.tavily.api_key` in `kiliax.yaml`, fallback `TAVILY_API_KEY`)

### crates/kiliax-llm (LLM facade + providers)

- Provider-neutral LLM facade, provider API routing, shared LLM error classification, and telemetry hook interface: `crates/kiliax-llm/src/lib.rs`, `crates/kiliax-llm/src/telemetry.rs`
- Protocol types (messages/tool-calls/usage/stream chunks) + provider-safe tool-name aliasing: `crates/kiliax-llm/src/types.rs`, `crates/kiliax-llm/src/tool_names.rs`
- OpenAI-compatible Chat Completions client + BYOT compatibility (streaming/tool-calls/usage + provider quirks like Moonshot/Kimi `reasoning_content`): `crates/kiliax-llm/src/openai_*.rs`, `crates/kiliax-llm/src/byot.rs`, `crates/kiliax-llm/src/patches.rs`
- OpenAI Responses API provider (request conversion, SSE events, prompt cache key forwarding, DashScope session-cache header + usage fallback, function-call/reasoning item replay + function-tool aliasing): `crates/kiliax-llm/src/openai_responses.rs`
- Anthropic Messages API provider (non-streaming + SSE/tool-use mapping + grouped tool_result request blocks + parallel tool-use controls): `crates/kiliax-llm/src/anthropic.rs`

### crates/kiliax-cli (TUI)

- UI + event loop + slash commands + session bootstrap: `crates/kiliax-cli/src/main.rs`
- Slash command definitions + popup: `crates/kiliax-cli/src/slash_command.rs`
- App state + render pipeline + token usage display + session-local settings changes: `crates/kiliax-cli/src/app/`
- Terminal init + backend: `crates/kiliax-cli/src/terminal.rs`
- Server daemon control: `crates/kiliax-cli/src/daemon.rs`

### crates/kiliax-server (HTTP control plane)

- Runner (`kiliax server run`): `crates/kiliax-server/src/runner.rs`
- HTTP router/handlers/auth/logs/WS/SSE/OpenAPI/web asset selection/session actions: `crates/kiliax-server/src/http/`
- HTTP <-> state domain mappers: `crates/kiliax-server/src/http/mapper.rs`
- State (config/session lifecycle/run queue/durable-vs-ephemeral events/tmp workspace cleanup/default persistence): `crates/kiliax-server/src/state/`
- State domain types (events/status/snapshots/runs/messages): `crates/kiliax-server/src/state/domain.rs`
- Infra (path validation/tmp workspace helpers/workspace hooks/external launchers + terminal cwd normalization): `crates/kiliax-server/src/infra.rs`
- REST/OpenAPI schemas (includes message `usage` and session default writes): `crates/kiliax-server/src/api.rs`
- OpenAPI metadata: `crates/kiliax-server/src/openapi.rs`

### crates/kiliax-otel (OpenTelemetry)

- OTLP exporters/providers + tracing/metrics/logs wiring: `crates/kiliax-otel/src/lib.rs`

### web (React UI)

- Main UI (responsive layout + WS streaming/session actions): `web/src/app.tsx`
- Message rendering + user input collapse controls + queued user bubble styling: `web/src/components/message-row.tsx`
- Dialog components: `web/src/components/*-dialog.tsx`
- Folder picker + path entry UX: `web/src/components/folder-picker.tsx`
- Action sheet/menu components: `web/src/components/*-actions.tsx`
- UI primitives (Dialog/Sheet/Button/Input/etc): `web/src/components/ui/`
- Build + dev server (Vite config/proxy): `web/vite.config.ts`
- API client + explicit session default persistence + display formatters: `web/src/lib/api.ts`, `web/src/lib/app-utils.ts`
- Alert/toast state: `web/src/lib/use-alerts.ts`
- Types (includes message `usage` and session default writes): `web/src/lib/types.ts`

## constraints
**All UI languages default to English, including any prompts, outputs, etc.**

### API

- Symptom: `HTTP 400 ... thinking is enabled but reasoning_content is missing in assistant tool call message at index N`.
- Why: Moonshot/Kimi validates the full `messages[]` history; when thinking is enabled, every `assistant` message that contains `tool_calls` must include a non-empty `reasoning_content`.
- Rule: always send `reasoning_content` for `assistant` tool-call messages; if there is no reasoning text, send a single whitespace (`" "`) (some gateways treat `""` as missing).
- Implementation: patch outbound JSON in `crates/kiliax-llm/src/patches.rs` (`inject_reasoning_content_for_tool_calls(...)`) and retry once when the provider returns this specific error.
- Provider routing: proxies may hide the upstream base_url/provider; detection should also match on the model string (see `should_inject_reasoning_content(...)`).

### TUI

Subsequent modifications must remain consistent:
- user bubble: history area rendering should match the input box style (no background + top/bottom padding)
- thinking: displayed in gray italics; can be streamed, but **must not** interleave with body output; body output should close/ignore subsequent thinking deltas once started
- status bar: at the bottom of the input box, showing status/agent_name/model_name
- compact output: avoid introducing extra blank lines (especially those caused by thinking/streaming rendering)
- follow a borderless design, simplify the UI

#### color
- prefer blue and purple highlights, followed by orange and green
- color usage scenarios: headers in purple; selected in blue, unselected in white; disabled in gray, enabled in white

### Web

- The session status badge (e.g. `step 1`) in the left sidebar must display in a single line and must not wrap to two lines


## dev ENV

[tui env](./ENV.md)  to check stty and $TERM

## ATTENTION

- After modifying code, update the arch section in AGENTS.md, only describing these core code parts
- Prioritize best practices, and take compatibility with common terminals into account (especially VSCode WSL / xterm.js)
- **Do not maintain compatibility with old interfaces or old code—delete and refactor directly**
