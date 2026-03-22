## description
一个高性能、强大、跨平台的AI Agent工具
subagents
token efficient
prompt caching


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
  - `plan.md`: plan agent 提示词（只读/受限命令）
  - `build.md`: build agent 提示词（可写/可执行）
  - `tools.md`: 通用工具使用规范（read/write/shell/mcp 命名空间）
- `crates/kiliax-core/examples/`: 可运行示例
  - `chat_hello.rs`: 非流式 chat 示例
  - `stream_chat.rs`: 流式 chat 示例（展示 delta 与 tool_call delta 合并）
  - `agent_loop.rs`: AgentRuntime + PromptBuilder 的闭环示例（流式输出 + 自动执行工具）
- `crates/kiliax-core/src/lib.rs`: 模块导出入口
- `crates/kiliax-core/src/config.rs`: 配置查找/解析（优先级路径）、provider/base_url/api_key、`<provider>/<model>` 路由；agent 运行参数（`runtime`/`agents.*`）
- `crates/kiliax-core/src/llm.rs`: OpenAI-compatible 客户端封装；消息/工具定义；流式通过 `byot` 解析 provider 扩展 `reasoning_content/thinking` → `ChatStreamChunk.thinking_delta`
- `crates/kiliax-core/src/agents.rs`: `AgentProfile`（plan/build）及其可用工具集合与权限模型
- `crates/kiliax-core/src/prompt.rs`: `PromptBuilder`（组装 system 前缀：agent prompt + tools 规范 + workspace root + `<skills_instructions>` skills 列表 + 对话消息）
- `crates/kiliax-core/src/runtime.rs`: `AgentRuntime`（ReAct/tool-calling 闭环；流式 run 支持取消）；转发 `thinking_delta` 为 `AgentEvent::AssistantThinkingDelta`
- `crates/kiliax-core/src/session.rs`: session 持久化（目录式：`meta.json` + `snapshot.json` + `events.jsonl`，默认写入 `<workspace>/.killiax/sessions/<session_id>/`）
- `crates/kiliax-core/src/tools/`: 工具系统
  - `mod.rs`: 权限/错误类型；导出 `ToolEngine`
  - `builtin.rs`: 内置工具 `read/write/shell` 的 schema + 执行（write 生成 JSON 摘要并在小改动时附带 unified diff；read 允许 workspace + skills roots；shell argv allowlist）
  - `engine.rs`: 工具路由与统一执行（builtin vs MCP），以及 MCP 工具 definitions 注入
  - `mcp.rs`: MCP stdio hub（连接 server、列出 tools、`mcp__<server>__<tool>` 命名空间、调用工具）
  - `skills.rs`: skills 发现（扫描 roots；按 `id` 去重；解析 `SKILL.md` YAML front matter 的 `name/description`；剥离 front matter 得到正文）

### crates/kiliax-tui

TUI 交互式对话界面（ratatui + crossterm）：inline viewport（参考 codex）+ 终端 scrollback 历史；启动先插入 header（版本/模型/cwd），输入框从 header 之后开始并随输出自动下推；输入框支持自动换行与动态高度。

- `crates/kiliax-tui/Cargo.toml`: TUI 依赖（`ratatui`/`crossterm`/`pulldown-cmark`/`syntect` 等）
- `crates/kiliax-tui/src/main.rs`: 入口与事件循环（键盘输入 + AgentRuntime 流）；每帧同步终端宽度到 `App` 供 thinking 软换行流式渲染
- `crates/kiliax-tui/src/app.rs`: `App` 状态（turn/step/tool 计时；`AssistantThinkingDelta` 灰色斜体输出，按终端宽度软换行逐行流式；正文开始后关闭/忽略 thinking 以避免混入正文；正文去掉多余前导空行；StepStart 先写入 Thinking 行再流式插入 AssistantDelta；统计输出 token 并用于 status/divider；工具调用折叠与 write diff 渲染）
- `crates/kiliax-tui/src/ui.rs`: codex 风格 composer（左侧 `›` 前缀、自动换行、动态高度）；输入框上方状态行显示计时 + token（当前 tool/step）；底部 footer（model/status/快捷键）
- `crates/kiliax-tui/src/header.rs`: 启动信息栏（版本/模型/cwd）渲染为 history lines
- `crates/kiliax-tui/src/style.rs`: composer 灰底样式与 diff 行背景（从终端默认背景色推导，类似 codex）
- `crates/kiliax-tui/src/markdown.rs`: Markdown 渲染（紧凑输出：不额外插入空行；pulldown-cmark → ratatui `Line`）；fenced code block 调用语法高亮；包含渲染紧凑性相关单元测试
- `crates/kiliax-tui/src/highlight.rs`: 代码语法高亮（syntect → ratatui spans）
- `crates/kiliax-tui/src/wrap.rs`: styled 文本按终端宽度换行
- `crates/kiliax-tui/src/input.rs`: 单行输入编辑（cursor/backspace/delete 等）；支持整行替换（历史回填）
- `crates/kiliax-tui/src/terminal.rs`: inline viewport backend（viewport 可下推/可伸缩；raw mode + bracketed paste）；仅重绘 viewport；跳过绘制终端右下角单元格以规避部分终端的滚屏/空行问题；`ViewportBackend` 也实现 `Write`，history 插入复用同一 backend writer（避免与 ratatui 绘制输出交错）
- `crates/kiliax-tui/src/history.rs`: 向 viewport 之上插入历史行（展开渲染 user bubble 与 turn divider marker；user bubble 使用与输入框一致的灰底样式并保留上下 padding；支持 fg/bg/italic 等 Style；渲染宽度预留 1 列避免 xterm.js 末列自动换行；codex 风格：用 DECSTBM 设置 scroll region，先用 RI(`ESC M`) 下推 viewport，再在上方 scroll region 底部用 CRLF(`\\r\\n`) 逐行插入；插入时临时关闭 wraparound + 过滤 `\\r/\\n`；必要时清理 continuation rows 以避免残留/空行）

## ATTENTION

- 修改完代码后，更新AGENTS.md中的arch部分，只用说明这些核心代码部分
- 优先遵循最佳实践，并兼顾常见终端兼容性（尤其 VSCode WSL / xterm.js），遗留代码可大胆重构
- 不要修改TODO.md
- TUI UI 约束（后续修改必须保持一致）：
  - user bubble：历史区渲染需与输入框风格一致（灰底 + 上下 padding）
  - thinking：以灰色斜体显示；可以流式，但**不得**与正文输出交织；正文开始后应关闭/忽略后续 thinking delta
  - 输出紧凑：避免引入多余空行（尤其是 thinking/流式渲染导致的空行）
