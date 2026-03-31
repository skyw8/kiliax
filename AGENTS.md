## description
a strong, high performance, cross-platform AI Agent tool
subagents
token efficient
minimal

## philosophy

- keep it simple, stupid
- less is more

## tech stack

- rust
- [rust-sdk](https://github.com/modelcontextprotocol/rust-sdk) MCP
- [async-openai](https://github.com/64bit/async-openai) openai compatible API
- [ratatui](https://github.com/ratatui/ratatui) TUI


## arch

### crates/kiliax-core (core library)

- Config + model routing: `crates/kiliax-core/src/config.rs`
- OpenAI-compatible client + streaming/tool-calls + thinking: `crates/kiliax-core/src/llm.rs`
- Prompt assembly (env/tools/skills/project `AGENTS.md`): `crates/kiliax-core/src/prompt.rs`
- Agent runtime (tool loop, parallel tool calls, streaming events): `crates/kiliax-core/src/runtime.rs`
- Session store (`meta.json`/`snapshot.json`/`events.jsonl`): `crates/kiliax-core/src/session.rs`
- Tool engine + builtin tools + MCP + skills discovery: `crates/kiliax-core/src/tools/`

### crates/kiliax-otel (OpenTelemetry)

- OTLP exporters/providers + tracing/metrics/logs wiring: `crates/kiliax-otel/src/lib.rs`

### crates/kiliax-cli (TUI)

- Ratatui UI + event loop + slash commands: `crates/kiliax-cli/src/main.rs`
- App state + render pipeline: `crates/kiliax-cli/src/app.rs`

### crates/kiliax-server (HTTP control plane)

- Axum routes + auth + static web hosting: `crates/kiliax-server/src/main.rs`
- Session lifecycle + settings (`settings.json`) + run queue + WS/SSE events: `crates/kiliax-server/src/state.rs`
- REST schema: `crates/kiliax-server/src/api.rs` (includes global `config.skills.*`)
- Key endpoints:
  - `POST /v1/sessions/{id}/fork` (fork at an assistant message and rerun the preceding user turn)
  - `GET /v1/config/skills` + `PATCH /v1/config/skills` (global per-skill enable settings)
  - `GET /v1/fs/list` (server-side folder browser for the web picker)
  - `POST /v1/sessions/{id}/open` (open workspace in `vscode` / `file_manager` / `terminal`)

### web (React UI)

- Main UI + WS streaming + folder picker + message fork + workspace open buttons + skills toggle: `web/src/app.tsx`
- API client: `web/src/lib/api.ts`
- Types: `web/src/lib/types.ts`

## constraints
**所有界面语言默认为英文，包括任何提示、输出等**

### TUI

后续修改必须保持一致
- user bubble：历史区渲染需与输入框风格一致（无背景 + 上下 padding）
- thinking：以灰色斜体显示；可以流式，但**不得**与正文输出交织；正文开始后应关闭/忽略后续 thinking delta
- status bar： 在输入框底部，显示status/agent_name/model_name
- 输出紧凑：避免引入多余空行（尤其是 thinking/流式渲染导致的空行）
- 遵循无框线的设计，简化UI

#### color
- 优先蓝色、紫色高亮，其次橙色绿色
- 颜色使用场景：题头为紫色；选中为蓝色，未选中为白色；未开启的显示为灰色，开启显示为白色

### Web

- 左侧导航栏的 session 状态 badge（如 `step 1`）必须单行显示，不得换行占两行

## ENV

终端环境
```
(base) skywo@skyw:~/github/kiliax$ stty -a
speed 38400 baud; rows 55; columns 89; line = 0;
intr = ^C; quit = ^\; erase = ^?; kill = ^U; eof = ^D; eol = M-^?; eol2 = M-^?;
swtch = <undef>; start = ^Q; stop = ^S; susp = ^Z; rprnt = ^R; werase = ^W; lnext = ^V;
discard = ^O; min = 1; time = 0;
-parenb -parodd -cmspar cs8 hupcl -cstopb cread -clocal -crtscts
-ignbrk brkint -ignpar -parmrk -inpck -istrip -inlcr -igncr icrnl ixon -ixoff -iuclc
ixany imaxbel iutf8
opost -olcuc -ocrnl onlcr -onocr -onlret -ofill -ofdel nl0 cr0 tab0 bs0 vt0 ff0
isig icanon iexten echo echoe echok -echonl -noflsh -xcase -tostop -echoprt echoctl
echoke -flusho -extproc
(base) skywo@skyw:~/github/kiliax$ echo $TERM
xterm-256color
```

## ATTENTION

- 修改完代码后，更新AGENTS.md中的arch部分，只用说明这些核心代码部分
- 优先遵循最佳实践，并兼顾常见终端兼容性（尤其 VSCode WSL / xterm.js）
- **不要兼容旧接口、旧代码等，直接删除重构即可**
