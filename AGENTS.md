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

- Agent profiles + tool permissions: `crates/kiliax-core/src/agents/`
- Config + model routing: `crates/kiliax-core/src/config.rs`
- OpenAI-compatible client + streaming/tool-calls + `prompt_cache_key` + per-call token `usage` capture: `crates/kiliax-core/src/llm.rs`
- Prompt assembly (model/agent/tools/skills/project + env last): `crates/kiliax-core/src/prompt.rs`
- Agent runtime (tool loop, parallel tool calls, streaming events, attach `usage` to assistant messages): `crates/kiliax-core/src/runtime.rs`
- Session store (`meta.json`/`snapshot.json`/`events.jsonl`) + message edit/truncate + snapshot self-heal + `prompt_cache_key` + persisted message `usage`: `crates/kiliax-core/src/session.rs`
- Tool engine + builtin tools + MCP + skills discovery (stable ordering): `crates/kiliax-core/src/tools/`

### crates/kiliax-cli (TUI)

- Ratatui UI + event loop + slash commands: `crates/kiliax-cli/src/main.rs`
- App state + render pipeline (+ per-call token usage display): `crates/kiliax-cli/src/app.rs`
- Terminal init + viewport backend: `crates/kiliax-cli/src/terminal.rs`
- Server daemon control (start/stop/restart): `crates/kiliax-cli/src/daemon.rs`

### crates/kiliax-server (HTTP control plane)

- Server entrypoint + config load + graceful shutdown: `crates/kiliax-server/src/main.rs`
- Router + handlers + auth/access log + WS/SSE events: `crates/kiliax-server/src/http/`
- App state (ArcSwap config) + session lifecycle + run queue + events log + limits: `crates/kiliax-server/src/state.rs`
- Infra (path validation + open workspace hooks): `crates/kiliax-server/src/infra.rs`
- REST schema: `crates/kiliax-server/src/api.rs` (includes global `config.providers.*` / `config.runtime.*` / `config.skills.*` + message `usage`)

### crates/kiliax-otel (OpenTelemetry)

- OTLP exporters/providers + tracing/metrics/logs wiring: `crates/kiliax-otel/src/lib.rs`

### web (React UI)

- Main UI + WS streaming + session fork + message edit/regenerate (via runs) + per-call token usage display + folder picker dialogs (`FolderPicker`, `FolderPickerDialog`) + settings (providers/models/api-key + agent max steps + raw YAML): `web/src/app.tsx`
- API client: `web/src/lib/api.ts`
- Types (includes message `usage`): `web/src/lib/types.ts`

## constraints
**All UI languages default to English, including any prompts, outputs, etc.**

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
