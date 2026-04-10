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


## arch

### crates/kiliax-core (core library)

- Agents + tool permissions: `crates/kiliax-core/src/agents/`
- Config + model routing: `crates/kiliax-core/src/config.rs`
- OpenAI-compatible client + BYOT compatibility (streaming/tool-calls/usage + provider quirks like Moonshot/Kimi `reasoning_content`): `crates/kiliax-core/src/llm.rs`
- Prompt assembly + nested project instruction scoping: `crates/kiliax-core/src/prompt.rs`
- Agent runtime loop + tool scheduling barriers + thinking/body normalization: `crates/kiliax-core/src/runtime.rs`
- Session store + snapshots + events + session-scoped MCP overrides: `crates/kiliax-core/src/session.rs`
- Tools (builtin patch application/MCP dispatch/skills discovery): `crates/kiliax-core/src/tools/`

### crates/kiliax-cli (TUI)

- UI + event loop + slash commands + session bootstrap: `crates/kiliax-cli/src/main.rs`
- App state + render pipeline + session-local settings changes: `crates/kiliax-cli/src/app.rs`
- Terminal init + backend: `crates/kiliax-cli/src/terminal.rs`
- Server daemon control: `crates/kiliax-cli/src/daemon.rs`

### crates/kiliax-server (HTTP control plane)

- Runner (`kiliax server run`): `crates/kiliax-server/src/runner.rs`
- HTTP router/handlers/auth/logs/WS/SSE/OpenAPI/web asset selection/session actions: `crates/kiliax-server/src/http/`
- State (config/session lifecycle/run queue/durable-vs-ephemeral events/tmp workspace cleanup/default persistence): `crates/kiliax-server/src/state.rs`
- Infra (path validation/tmp workspace helpers/workspace hooks/launch normalization): `crates/kiliax-server/src/infra.rs`
- REST/OpenAPI schemas (includes message `usage` and session default writes): `crates/kiliax-server/src/api.rs`
- OpenAPI metadata: `crates/kiliax-server/src/openapi.rs`

### crates/kiliax-otel (OpenTelemetry)

- OTLP exporters/providers + tracing/metrics/logs wiring: `crates/kiliax-otel/src/lib.rs`

### web (React UI)

- Main UI (WS streaming/session fork/edit/regenerate/usage/tmp workspace cleanup/session vs default settings/sidebar refresh): `web/src/app.tsx`
- Build + dev server (Vite config/proxy): `web/vite.config.ts`
- API client + explicit session default persistence: `web/src/lib/api.ts`
- Types (includes message `usage` and session default writes): `web/src/lib/types.ts`

## constraints
**All UI languages default to English, including any prompts, outputs, etc.**

### API

- Symptom: `HTTP 400 ... thinking is enabled but reasoning_content is missing in assistant tool call message at index N`.
- Why: Moonshot/Kimi validates the full `messages[]` history; when thinking is enabled, every `assistant` message that contains `tool_calls` must include a non-empty `reasoning_content`.
- Rule: always send `reasoning_content` for `assistant` tool-call messages; if there is no reasoning text, send a single whitespace (`" "`) (some gateways treat `""` as missing).
- Implementation: patch outbound JSON in `crates/kiliax-core/src/llm.rs` (`inject_reasoning_content_for_tool_calls(...)`) and retry once when the provider returns this specific error.
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

VSCode WSL2 ubuntu
Terminal
```
(base) skywo@skyw:~/github/kiliax$ stty -a
speed 38400 baud; rows 55; columns 89; line = 0;
intr = ^C; quit = ^\; erase = ^?; kill = ^U; eof = ^D; eol = M-^?; eol2 = M-^?;
swtch = <undef>; start = ^Q; stop = ^S; susp = ^Z; rprnt = ^R; werase = ^W; lnext = ^V;
discard = ^O; min = 1; time = 0;
-parenb -parodd -cmspar cs8 hupcl -cstopb cread -clocal -crtscts
-ignbrk brkint -ignpar -parmrk -inpck -istrip -inlcr -igncr icrnl ixon -ixoff -iuclc
ixany imaxbel iutf8
opost -oluc -ocrnl onlcr -onocr -onlret -ofill -ofdel nl0 cr0 tab0 bs0 vt0 ff0
isig icanon iexten echo echoe echok -echonl -noflsh -xcase -tostop -echoprt echoctl
echoke -flusho -extproc
(base) skywo@skyw:~/github/kiliax$ echo $TERM
xterm-256color
```

## ATTENTION

- After modifying code, update the arch section in AGENTS.md, only describing these core code parts
- Prioritize best practices, and take compatibility with common terminals into account (especially VSCode WSL / xterm.js)
- **Do not maintain compatibility with old interfaces or old code—delete and refactor directly**
