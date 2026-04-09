# Refactor Report (2026-04-03)

This document records the original architecture problems (P0/P1), the refactor design chosen, and the concrete implementation shipped in the repository.

## Scope

- Repo: `kiliax`
- Focus: session settings vs global defaults, tool/mcp concurrency, streaming/event durability, web asset serving, LLM transport, project instruction scoping.
- Not in scope: large-scale file/module splitting (not required to land the functional fixes).

## Executive Summary

- Introduced a clear control-plane contract:
  - **Session changes are session-local**.
  - **Persisting global defaults is explicit** (`save-defaults` endpoint).
- Fixed multiple correctness/performance hazards:
  - `apply_patch` is no longer parallelized and is executed with a two-phase plan/apply flow.
  - MCP calls no longer hold a hub-wide mutex across `await`.
  - Streaming deltas are no longer persisted to the durable event log; durable vs ephemeral events are separated.
  - “Thinking must not interleave with body output once body starts” is enforced upstream (producer side).
- Fixed architectural debt:
  - Runtime `web/dist` now takes precedence over embedded assets (server dist fallback test passes).
  - LLM reuses a shared `reqwest::Client` and preserves tool `strict`.
  - Prompt building supports nested `AGENTS.md` / `CLAUDE.md` scoping.
- Validation: `cargo test -p kiliax-core`, `cargo test -p kiliax-server`, `cargo test -p kiliax`, and `web` production build are green.

## Baseline Architecture (Before)

### Settings source-of-truth ambiguity

- CLI implemented “switch model” / “toggle MCP” by editing `kiliax.yaml` (global config file).
- Server/Web had session-local settings but also contained implicit global sync paths, resulting in two competing sources of truth.

### Event model conflation

- Streaming deltas (`assistant_delta`, `assistant_thinking_delta`) were treated as durable events:
  - appended + flushed per delta, amplifying I/O.
  - recovery depended on scanning/reading the durable log.
- Protocol constraints (“thinking stops when body starts”) were implemented in consumers (Web/TUI), not in the producer.

## Target Architecture (After)

### Three layers of configuration

1) **Global config**
   - provider definitions, API keys, MCP server definitions/credentials
   - `default_model`
   - default MCP enable flags
2) **Session settings**
   - `agent`, `model_id`
   - session-scoped MCP enablement set
   - `workspace_root`, `extra_workspace_roots`
3) **Run overrides**
   - one-run-only overrides (kept as-is; key fix is preventing implicit global writes)

### Explicit “save defaults”

- `PATCH /v1/sessions/{id}/settings` updates session only.
- `POST /v1/sessions/{id}/settings/save-defaults` explicitly persists session’s model and/or MCP enablement back to global defaults.

### Durable vs ephemeral events

- Durable events are persisted to the session event log.
- Ephemeral events are broadcast + kept in the in-memory ring buffer only (not persisted).

## Issues → Refactor Design → Implemented Changes

### P0-1: CLI uses global config edits for “current session” model/MCP

**Original problem**

- CLI implemented session changes by writing global config (`kiliax.yaml`).
- Server/Web had session-local settings, but behavior wasn’t consistently enforced and default syncing was implicit.

**Risk**

- Multiple sessions/windows race on the same config file.
- “Change this session” unexpectedly changes defaults for future sessions.
- Web and CLI can disagree about effective state; hard to reproduce historical behavior.

**Refactor design**

- Session changes always mutate session state only.
- Add a separate explicit action to persist defaults back to global config.

**Implemented**

- Server:
  - Request schema: `crates/kiliax-server/src/api.rs` (`SessionSaveDefaultsRequest`).
  - Endpoint: `crates/kiliax-server/src/http/mod.rs` (`POST /v1/sessions/{id}/settings/save-defaults`).
  - State implementation: `crates/kiliax-server/src/state.rs` (`save_session_defaults`).
  - Removed implicit “patch session settings also updates global default_model”.
- CLI:
  - Introduced session-scoped MCP enablement persistence (`SessionMeta.mcp_servers`) in `crates/kiliax-core/src/session.rs`.
  - Session-local model/MCP toggles without writing YAML: `crates/kiliax-cli/src/app.rs`, `crates/kiliax-cli/src/main.rs`.
- Web:
  - Session change paths patch session only; explicit “Save default” actions call `save-defaults`.

**Commits**

- `caa233d` Refactor session defaults handling
- `bf551de` Make CLI settings session-local

### P0-2: `apply_patch` marked parallel + non-transactional

**Original problem**

- `apply_patch` was treated as parallel-safe, but it mutates files directly and could partially apply then fail.

**Risk**

- Concurrent patch calls interleave writes.
- Failure mid-patch leaves dirty workspace state.

**Refactor design**

- Treat `apply_patch` as an exclusive barrier.
- Two-phase execution: plan/validate before writing; rollback best-effort if apply fails.

**Implemented**

- Tool scheduling: `apply_patch` is `Exclusive` in `crates/kiliax-core/src/tools/mod.rs`.
- Patch execution:
  - plan final states for all touched paths
  - validate full patch upfront
  - apply changed paths; rollback on failure
  - test ensures no writes happen if later op fails
  - implementation in `crates/kiliax-core/src/tools/builtin/apply_patch.rs`
- Runtime test ensures `apply_patch` acts as a scheduling barrier (`crates/kiliax-core/src/runtime.rs`).

**Commits**

- `9730c7b` Make apply_patch exclusive and unblock MCP calls

### P0-3: MCP hub lock held across `await`

**Original problem**

- MCP tool dispatch held `servers` mutex across remote `await`.

**Risk**

- One slow MCP server blocks other MCP server calls.

**Refactor design**

- Lock only long enough to get the `Arc<McpServer>` handle, then drop lock before `await`.

**Implemented**

- `crates/kiliax-core/src/tools/mcp.rs` clones server handle out of the mutex before `call_tool().await`.

**Commits**

- `9730c7b` Make apply_patch exclusive and unblock MCP calls

### P0-4: Streaming deltas persisted + flushed per delta; recovery is linear; web polls sessions every second

**Original problem**

- Delta events were appended + flushed per event.
- Durable log size grows quickly; recovery is expensive.
- Web polled sessions list every second even with WS events available.

**Risk**

- Excessive I/O and CPU under long outputs.
- Slow restarts/reconnects with large event logs.
- Unnecessary request load.

**Refactor design**

- Split event pipeline into durable vs ephemeral:
  - deltas are ephemeral only
  - durable events are persisted
- Make web refresh event-driven + low-frequency fallback.

**Implemented**

- Server:
  - `EventPersistence { Durable, Ephemeral }` in `crates/kiliax-server/src/state.rs`.
  - `assistant_delta` / `assistant_thinking_delta` emitted via `emit_ephemeral_event` (not appended).
  - Durable events continue to append.
  - Test: `streaming_events_stay_in_memory_until_message_is_finalized`.
- Web:
  - Reduced polling to 10 seconds.
  - Added `refreshSessionsIfStale()` and triggers refresh on meaningful events.
  - Changes in `web/src/app.tsx`.

**Commits**

- `afe3892` Keep streaming deltas ephemeral

### P0-5: “thinking must stop once body starts” enforced in consumers, not producer

**Original problem**

- Web/TUI filtered thinking deltas after body started, but runtime still produced them.

**Risk**

- Wasted events/bandwidth, and new clients may violate the rule.

**Refactor design**

- Enforce upstream in the producer: once body starts, discard subsequent thinking deltas.

**Implemented**

- Runtime filters thinking deltas after body begins (`assistant_body_started`) in `crates/kiliax-core/src/runtime.rs`.

**Commits**

- `afe3892` Keep streaming deltas ephemeral

### P1-6: Embedded web assets short-circuit runtime `web/dist`

**Original problem**

- If embedded assets are enabled at build time, runtime `web/dist` was ignored (test expected runtime dist serving).

**Refactor design**

- Runtime precedence must be: `web/dist` first, then embedded fallback, then hint page.

**Implemented**

- `crates/kiliax-server/src/http/mod.rs`: check `find_web_dist_dir()` first; only use embedded if dist is not found.

**Commits**

- `89aa8d0` Prefer runtime web dist and reuse LLM HTTP client

### P1-7: LLM creates a new `reqwest::Client` per call; tool `strict` dropped

**Original problem**

- `chat` and `chat_stream` created a fresh `reqwest::Client`, losing connection pooling.
- `ToolDefinition.strict` was always dropped for OpenAI schema conversion.

**Refactor design**

- LLM client owns a shared `reqwest::Client` per instance.
- Preserve `strict` in tool schema.

**Implemented**

- `crates/kiliax-core/src/llm.rs`:
  - `LlmClient` now contains `http: reqwest::Client`.
  - `chat` and `chat_stream` reuse it.
  - `to_openai_tool()` now sets `strict: tool.strict`.
  - Updated unit test to assert strict is preserved.

**Commits**

- `89aa8d0` Prefer runtime web dist and reuse LLM HTTP client

### P1-8: Project instructions read only root `AGENTS.md/CLAUDE.md`

**Original problem**

- Prompt builder read only `workspace_root/AGENTS.md` or `workspace_root/CLAUDE.md`.

**Refactor design**

- Walk ancestors root→leaf, and include nested instruction files in scope order.
- Prefer `AGENTS.md` over `CLAUDE.md` per directory.

**Implemented**

- `crates/kiliax-core/src/prompt.rs`:
  - renders `# Project Instructions` with per-file `## <path>` sections.
  - adds `project_instruction_paths()` and tests ordering.

**Commits**

- `779a3ff` Scope project prompts by nested instruction files

### P1-9: Hotspot files (maintainability)

**Original problem**

- `crates/kiliax-server/src/state.rs`, `crates/kiliax-cli/src/app.rs`, `web/src/app.tsx` are large and multi-responsibility.

**Status**

- Not fully split in this landing (functional refactors were prioritized), but the changes introduce stable seams:
  - explicit defaults persistence API
  - durable/ephemeral event boundary
  - session-local config helpers

**Recommended follow-up**

- Server: split `state.rs` into `config`, `sessions`, `events`, `runs`.
- CLI: split `app.rs` into `ui_state`, `settings_actions`, `stream_reducer`, `persistence`.
- Web: split `app.tsx` into `transport`, `session store`, `settings panels`, `chat view`.

## Public API Changes

- New endpoint:
  - `POST /v1/sessions/{session_id}/settings/save-defaults`
  - Body: `{ "model": true|false, "mcp": true|false }`
- Session patch semantics:
  - `PATCH /v1/sessions/{session_id}/settings` updates session only and does not write global defaults implicitly.

## Validation

- `cargo test -p kiliax-core`
- `cargo test -p kiliax-server`
- `cargo test -p kiliax`
- `cd web && npm run build`

## Commits (in order)

- `caa233d` Refactor session defaults handling
- `bf551de` Make CLI settings session-local
- `9730c7b` Make apply_patch exclusive and unblock MCP calls
- `afe3892` Keep streaming deltas ephemeral
- `89aa8d0` Prefer runtime web dist and reuse LLM HTTP client
- `779a3ff` Scope project prompts by nested instruction files
