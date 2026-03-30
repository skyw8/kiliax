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

### crates/kiliax-core

核心库：配置、LLM(OpenAI-compatible)、agents、prompt、tools、runtime、session。

- `crates/kiliax-core/Cargo.toml`: core 依赖（`async-openai` 启用 `byot` 以兼容扩展字段、MCP client 等）
- `crates/kiliax-core/prompts/`: agent 提示词（markdown，编译期 `include_str!`）
  - `codex.md`: 模型层提示词（默认，用于 gpt 系列）
  - `plan.md`: plan agent 提示词（只读/受限命令）
  - `general.md`: general agent 提示词（可写/可执行）
  - `how_to_use_skills.md`: skills 使用规范（与 skills 列表一起注入）
  - `tools/*.md`: 内置工具提示词/说明（编译期 `include_str!`）
- `crates/kiliax-core/examples/`: 可运行示例
  - `chat_hello.rs`: 非流式 chat 示例
  - `stream_chat.rs`: 流式 chat 示例（展示 delta 与 tool_call delta 合并）
  - `agent_loop.rs`: AgentRuntime + PromptBuilder 的闭环示例（流式输出 + 自动执行工具）
- `crates/kiliax-core/src/lib.rs`: 模块导出入口
- `crates/kiliax-core/src/config.rs`: 配置查找/解析（默认仅 `~/.kiliax/kiliax.yaml`）、provider/base_url/api_key、model 路由（按首个 `/` 分割，支持 `openrouter/openai/gpt-4o-mini` 这类 model id）；多 provider 时可按 `providers.*.models` 反查唯一 provider；server 配置（`server.host/port/token`）；**OpenTelemetry 配置（`otel.*`，OTLP base endpoint 校验含 Langfuse 提示）**；agent 运行参数（`runtime`/`agents.*`）；工具配置（`web_search.*` 与兼容 `tools.tavily.*`）；MCP 配置（`mcp.servers`: `enable` + stdio command+args）；包含 config/resolve_model 单元测试
- `crates/kiliax-core/src/llm.rs`: OpenAI-compatible 客户端封装；消息/工具定义（assistant 支持 `reasoning_content` 以兼容部分 provider 的 tool loop）；User 输入支持 `UserMessageContent`（text + local image path → data URL base64；仅图片输入会自动补 `.` 文本占位避免 400 empty text）；chat/流式均用 `reqwest` 直连 `/chat/completions`（4xx 会读取 body 并转换为 `ApiError`）；解析 provider 扩展 `reasoning_content/thinking/reasoning` → `ChatStreamChunk.thinking_delta`；Moonshot 等 provider 在 assistant tool_calls 消息中要求携带 `reasoning_content` 时自动注入；**OTEL spans（Langfuse GenAI `gen_ai.*` + `langfuse.observation.*`；含 TTFT/TPS attrs）**
- `crates/kiliax-core/src/telemetry.rs`: 可观测性全局开关（随 Config/ToolEngine 热更新）；full capture（UTF-8 截断 + sha256）；OTEL metrics instruments（LLM / tools / MCP / skills / run）；`telemetry::spans` 用于写入 OTEL span attributes + 获取当前 `trace_id`（便于前后端串联排障）
- `crates/kiliax-core/src/agents/`: `AgentProfile`（plan/general）及其可用工具集合与权限模型（按 agent 拆分）
- `crates/kiliax-core/src/prompt.rs`: `PromptBuilder`（分层 system prompt；tools 说明按 `AgentProfile.tools` 动态渲染并标注并行能力；工具使用约束收敛到各 tool 的 description/parameters）
- `crates/kiliax-core/src/runtime.rs`: `AgentRuntime`（ReAct/tool-calling 闭环；支持并行执行可并行工具调用；tool_call_id 空/重复自动归一化；流式 run 支持取消；支持工具返回多条消息用于 image attach；每 step 请求前规整 tool_calls/Tool 消息序列（补齐缺失/保证顺序），避免 provider 因 tool_call_id 未响应报 400）；转发 `thinking_delta` 为 `AgentEvent::AssistantThinkingDelta`，并在 tool_calls step 中累积为 assistant 的 `reasoning_content` 以便下一轮回放；**OTEL `kiliax.agent.run` + per-step `kiliax.agent.step` spans（Langfuse observation.type=agent/chain）**
- `crates/kiliax-core/src/session.rs`: session 持久化（目录式：`meta.json` + `snapshot.json` + `events.jsonl`，默认写入 `~/.kiliax/sessions/<session_id>/`）；title 从首条 user 文本派生并做 UTF-8 安全截断（避免中文等多字节字符触发 panic）
- `crates/kiliax-core/src/tools/`: 工具系统
  - `mod.rs`: 权限/错误类型；导出 `ToolEngine`；定义 `ToolParallelism` 与并行能力判定
  - `builtin/`: codex 风格内置工具 schema + 执行（按工具拆分）
    - `mod.rs`: tool name 常量 + dispatcher + re-export
    - `common.rs`: args/path 解析（workspace/skills roots；拒绝 `..`，防 symlink escape）
    - `file_tracker.rs`: 文件读写追踪（要求先 `read_file` 再 `write_file`/`edit_file`；检测读后变更）
    - `read_file.rs`: `read_file`
    - `list_dir.rs`: `list_dir`
    - `grep_files.rs`: `grep_files`（ignore/grep-searcher；尊重 ignore）
    - `write_file.rs`: `write_file`
    - `edit_file.rs`: `edit_file`
    - `web_search.rs`: `web_search`（Tavily API；从 `kiliax.yaml` 的 `web_search.*` 加载，兼容 `tools.tavily.*` 与 env `TAVILY_API_KEY` / `TAVILY_API_BASE_URL`）
    - `view_image.rs`: `view_image`（读取并 attach 本地图片到下一轮上下文）
    - `shell.rs`: `shell_command`/`write_stdin`（argv allowlist + sessions）
    - `apply_patch.rs`: `apply_patch`（Begin/End Patch）
    - `update_plan.rs`: `update_plan`
  - `engine.rs`: 工具统一执行入口（维护 shell sessions；持有 Config 并支持 `set_config` 热更新；支持 tool 输出多条消息用于 image attach；MCP servers 后台连接/重试（指数 backoff），支持 `enable` 开关（禁用的不连接/不重试/调用时报错）；提供 `mcp_status` 快照给 UI）；**OTEL tool span + args/output 捕获 + metrics（Langfuse tool observation input/output）**
  - `mcp.rs`: MCP stdio hub（QuietStdioTransport：drain stderr 避免污染 TUI + `kill_on_drop` 防止失败连接遗留子进程；connect/list_tools 超时；`mcp__<server>__<tool>` 命名空间、调用工具；shutdown 超时；提供已连接 server 概览）；**OTEL connect/call spans + metrics**
  - `skills.rs`: skills 发现（扫描 roots；按 `id` 去重；解析 `SKILL.md` YAML front matter 的 `name/description`；剥离 front matter 得到正文）；**OTEL span + metrics（Langfuse observation.type=span）**

### crates/kiliax-otel

OpenTelemetry 初始化与导出（OTLP HTTP/gRPC）：按 `kiliax.yaml` 的 `otel.*` 配置安装 global tracer/logger/meter 与 tracing subscriber（HTTP exporter 会用 base endpoint 拼接 `/v1/{signal}`）；支持从 HTTP headers 继承 `traceparent/tracestate`。

- `crates/kiliax-otel/src/lib.rs`: exporters + providers（logs/traces/metrics）构建与 shutdown；`tracing-opentelemetry` / `OpenTelemetryTracingBridge` layers；`set_parent_from_http_headers`；本地日志支持 `Stdout`/`File`/`None`（`File` 失败不致命，best-effort）
- `crates/kiliax-otel/src/otlp.rs`: headers/TLS/reqwest client 构建（blocking/async）与 timeout 解析

### crates/kiliax-cli

TUI 交互式对话界面（ratatui + crossterm）：inline viewport（参考 codex）+ 终端 scrollback 历史；启动先插入 header（版本/模型/cwd），输入框从 header 之后开始并随输出自动下推；输入框支持自动换行与动态高度。

- `crates/kiliax-cli/Cargo.toml`: TUI 依赖（`ratatui`/`crossterm`/`pulldown-cmark`/`syntect`/`reqwest` 等）
- `crates/kiliax-cli/src/main.rs`: CLI 启动参数（`--help/--version`、profile override、`--resume`、`serve start|stop|restart`）；入口与事件循环（键盘输入 + AgentRuntime 流 + message queue 自动串行发送；过滤 `KeyEventKind::Release` 避免 Windows 按键重复）；slash command 分发（/new、/agent、/model、/mcp）与模型切换落盘；退出时若未发送 user 消息则删除 session（不输出 resume 提示）；不再自动拉起 `kiliax-server`；**OTEL 初始化 + 默认落盘 `~/.kiliax/tui.log`（避免污染终端）**
- `crates/kiliax-cli/src/daemon.rs`: 后台 `kiliax-server` 管理（`kiliax serve start|stop|restart`；健康检查；启动时校验 web root 可用性，不可用则尝试 stop 并重启；开发态优先通过 `cargo run -p kiliax-server` 确保与源码同步）；状态写入 `~/.kiliax/server.json`，日志写入 `~/.kiliax/server.log`
- `crates/kiliax-cli/src/app.rs`: `App` 状态（stream collector/不交织 thinking；turn/step/tool 计时；工具调用折叠展示（`shell_command` 仅展示关键命令/参数，省略 bash -lc/cd/env 等包装与冗长参数）；`update_plan` 以 `[]` 待办/完成删除线展示（不显示 pending 等状态字样）；图片附件以输入框内联 token `[img#N]` 形式挂载/删除；提交时自动剥离 token 仅发送图片；message queue：运行中提交入队、Ctrl+C 撤回、↑ 回溯编辑；提供队列预览数据给 UI）；slash command（/new、/agent、/model、/mcp）与 UI mode（chat/model picker/mcp picker）状态机；/model 切换会更新 `kiliax.yaml` 的 `default_model` 并热切换 runtime（reload Config + `ToolEngine::set_config`）；/mcp 通过 TUI 开关写回 `mcp.servers[].enable`（YAML 行编辑，正确处理 `#` 注释/引号）并 checkpoint session（刷新 system preamble）；错误展示支持 error-chain（多行）并写入 tracing 日志
- `crates/kiliax-cli/src/ui.rs`: codex 风格 composer（无背景；蓝+紫 `››` 前缀；自动换行、动态高度；`[img#N]` token 蓝色高亮；输入框上方 queue 预览）；/model、/mcp 等 picker（题头紫色、选中蓝色、未选中白色；MCP 未开启灰色/开启白色）；底部 footer 仅显示 status + agent + model_id（去 provider）
- `crates/kiliax-cli/src/header.rs`: 启动信息栏（版本/模型/cwd）渲染为 history lines
- `crates/kiliax-cli/src/clipboard_paste.rs`: 剪切板图片读取→临时 PNG 路径（arboard + WSL PowerShell fallback）；粘贴路径规范化（file://、Windows/UNC、WSL 映射）
- `crates/kiliax-cli/src/slash_command.rs`: slash command 定义 + popup 状态（/new、/agent、/model、/mcp；↑/↓ 选择、Tab 补全、/a alias；popup 高度按 item 数，UI 无边框）
- `crates/kiliax-cli/src/mcp_picker.rs`: MCP servers 开关选择器（↑/↓ 选择、Space/Enter 切换、Esc 关闭）
- `crates/kiliax-cli/src/model_picker.rs`: model picker 状态（provider/model 列表、模糊搜索、键盘导航；返回值总是 provider-qualified model id，兼容带 `/` 的模型名）
- `crates/kiliax-cli/src/style.rs`: composer 无背景样式、prompt 双彩箭头配色（按终端主题 hint），以及 picker 统一配色（题头紫色、选中蓝色、未选中白色；MCP disabled 灰色）与 diff 行背景
- `crates/kiliax-cli/src/markdown.rs`: Markdown 渲染（紧凑输出：不额外插入空行；pulldown-cmark → ratatui `Line`；不启用 GFM tables，表格按原文 `|` 展示以保证终端稳定性）；fenced code block 调用语法高亮；包含渲染紧凑性相关单元测试
- `crates/kiliax-cli/src/highlight.rs`: 代码语法高亮（syntect + VS Code Dark+ 配色；用 LinesWithEndings 保证注释/多行状态不串行，并剥离 CR/LF 避免渲染异常）
- `crates/kiliax-cli/src/wrap.rs`: styled 文本按终端宽度换行（含宽字符/样式保持单元测试）
- `crates/kiliax-cli/src/input.rs`: 单行输入编辑（cursor/backspace/delete 等）；`[img#N]` token 原子移动/删除；支持整行替换（历史回填）；包含 Unicode/快捷键单元测试
- `crates/kiliax-cli/src/custom_terminal.rs`: TUI 用到的自定义终端命令（scroll region、wraparound 开关、RI 等 ANSI 序列封装）
- `crates/kiliax-cli/src/terminal.rs`: inline viewport backend（viewport 可下推/可伸缩；raw mode + bracketed paste）；仅重绘 viewport；跳过绘制终端右下角单元格以规避部分终端的滚屏/空行问题；每帧用 Synchronized Update 包裹所有写入（同一 backend writer）以减少 tearing
- `crates/kiliax-cli/src/history.rs`: 向 viewport 之上插入历史行（展开渲染 user bubble 与 turn divider marker；user bubble 使用与输入框一致样式（无背景 + 上下 padding + 双彩箭头前缀）；支持 fg/bg/italic/underline/strikethrough 等 Style；渲染宽度预留 1 列避免 xterm.js 末列自动换行；codex 风格：用 DECSTBM 设置 scroll region，先用 RI(`ESC M`) 下推 viewport，再在上方 scroll region 底部用 CRLF(`\r\n`) 逐行插入；插入时临时关闭 wraparound + 过滤 `\r/\n`；必要时清理 continuation rows 以避免残留/空行；相关 ANSI 命令封装在 `custom_terminal`）；包含 marker 解析/user bubble/divider 展开单元测试

### crates/kiliax-server

Session 控制面：提供 REST + SSE/WS 事件流接口以创建/恢复 session、发送消息（run）、切换 agent/model/MCP，以及查询 messages/status/capabilities。`kiliax.yaml` 仅作为新 session 默认值；session 覆盖持久化到 `settings.json`。

- `crates/kiliax-server/Cargo.toml`: server 依赖（axum/ws/sse、tracing 等）
- `crates/kiliax-server/src/main.rs`: `/v1` REST + SSE/WS 路由；包含 `DELETE /v1/sessions/{id}`、`GET /v1/skills`、`PATCH /v1/config/mcp`；鉴权（API 支持 Bearer/Cookie/`?token=`，Web UI 通过 `?token=` 首次握手写入 HttpOnly cookie 并重定向）；静态托管 `web/dist`（SPA fallback 到 `index.html`）；**OTEL 初始化 + HTTP TraceLayer（traceparent 继承；`http.target` 会剥离 `?token=`）+ access log（单行）**
- `crates/kiliax-server/src/state.rs`: `ServerState`/`LiveSession`；run 队列串行执行；`delete_session` + `LiveSession::shutdown`（取消 run + 停止 worker）；`list_global_skills`；`patch_config_mcp`（更新 `kiliax.yaml` 并热更新 ToolEngine）；`PATCH /v1/sessions/{id}/settings` 切换 model 会同步写回 `kiliax.yaml.default_model`（影响后续新建 session 默认模型）；session settings（含 `workspace_root`）持久化到 `settings.json`；默认 tmp workspace 为 `~/.kiliax/workspace/tmp_<SessionId>`；支持 `/v1/config` 读写 YAML 并热更新 live sessions；skills 发现 `/v1/sessions/{id}/skills`；run/session 生命周期写入 info 日志，runtime error 记录 error-chain；**OTEL run span 写入 Langfuse trace-level attrs**
- `crates/kiliax-server/src/api.rs`: REST schema（session summary 增强：`updated_at/last_outcome`；新增 `config/skills/workspace_root` 相关结构；`ConfigMcpPatchRequest`、skills list 等）
- `crates/kiliax-server/src/error.rs`: 统一错误模型（`{ error: { code, message, details? }, trace_id? }`）；内部错误自动附带 `details.error_chain`；IntoResponse 会写入 structured tracing 日志
- `crates/kiliax-server/src/tests.rs`: server HTTP 行为测试（包含 session delete、global skills、config mcp patch）

### web

Web UI（React + Vite + Tailwind + shadcn/ui），由 `kiliax-server` 静态托管：

- `web/src/app.tsx`: 单页应用（顶部栏 Agent/Model/CWD；session list 懒加载；三点菜单 Pin/Delete + 删除确认弹窗；全局 Skills/MCP（kiliax.yaml）；Send/Interrupt；对话渲染；API 错误弹窗展示 `code/message/details/trace_id`）
- `web/src/components/markdown.tsx`: 轻量 Markdown 渲染（安全：不渲染 HTML；支持 GFM 表格/对齐）
- `web/src/lib/api.ts`: Web API client（API 错误解析：`code/message/details/trace_id`，并在 UI 侧可展示/复制；包含 `DELETE /v1/sessions/{id}`）

## constraints

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
