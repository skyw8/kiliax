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
  - `tools.md`: 工具使用规则（工具列表/并行能力由 `PromptBuilder` 动态注入）
  - `how_to_use_skills.md`: skills 使用规范（与 skills 列表一起注入）
- `crates/kiliax-core/examples/`: 可运行示例
  - `chat_hello.rs`: 非流式 chat 示例
  - `stream_chat.rs`: 流式 chat 示例（展示 delta 与 tool_call delta 合并）
  - `agent_loop.rs`: AgentRuntime + PromptBuilder 的闭环示例（流式输出 + 自动执行工具）
- `crates/kiliax-core/src/lib.rs`: 模块导出入口
- `crates/kiliax-core/src/config.rs`: 配置查找/解析（优先级路径）、provider/base_url/api_key、model 路由（按首个 `/` 分割，支持 `openrouter/openai/gpt-4o-mini` 这类 model id）；多 provider 时可按 `providers.*.models` 反查唯一 provider；agent 运行参数（`runtime`/`agents.*`）；工具配置（`web_search.*` 与兼容 `tools.tavily.*`）；包含 config 优先级/resolve_model 单元测试
- `crates/kiliax-core/src/llm.rs`: OpenAI-compatible 客户端封装；消息/工具定义；User 输入支持 `UserMessageContent`（text + local image path → data URL base64）；流式用 `reqwest-eventsource` 直连 SSE（4xx 会读取 body 并转换为 `ApiError`），并解析 provider 扩展 `reasoning_content/thinking` → `ChatStreamChunk.thinking_delta`；工具 schema 默认不发送 `strict` 以提升兼容性
- `crates/kiliax-core/src/agents/`: `AgentProfile`（plan/general）及其可用工具集合与权限模型（按 agent 拆分）
- `crates/kiliax-core/src/prompt.rs`: `PromptBuilder`（分层 system prompt；tools 说明按 `AgentProfile.tools` 动态渲染，并标注可并行工具）
- `crates/kiliax-core/src/runtime.rs`: `AgentRuntime`（ReAct/tool-calling 闭环；支持并行执行可并行工具调用；tool_call_id 空/重复自动归一化；流式 run 支持取消；支持工具返回多条消息用于 image attach）；转发 `thinking_delta` 为 `AgentEvent::AssistantThinkingDelta`
- `crates/kiliax-core/src/session.rs`: session 持久化（目录式：`meta.json` + `snapshot.json` + `events.jsonl`，默认写入 `<workspace>/.killiax/sessions/<session_id>/`）
- `crates/kiliax-core/src/tools/`: 工具系统
  - `mod.rs`: 权限/错误类型；导出 `ToolEngine`；定义 `ToolParallelism` 与并行能力判定
  - `builtin/`: codex 风格内置工具 schema + 执行（按工具拆分）
    - `mod.rs`: tool name 常量 + dispatcher + re-export
    - `common.rs`: args/path 解析（workspace/skills roots）
    - `read_file.rs`: `read_file`
    - `list_dir.rs`: `list_dir`
    - `grep_files.rs`: `grep_files`（ignore/grep-searcher；尊重 ignore）
    - `web_search.rs`: `web_search`（Tavily API；从 `killiax.yaml` 的 `web_search.*` 加载，兼容 `tools.tavily.*` 与 env `TAVILY_API_KEY` / `TAVILY_API_BASE_URL`）
    - `view_image.rs`: `view_image`（读取并 attach 本地图片到下一轮上下文）
    - `shell.rs`: `shell_command`/`write_stdin`（argv allowlist + sessions）
    - `apply_patch.rs`: `apply_patch`（Begin/End Patch）
    - `update_plan.rs`: `update_plan`
  - `engine.rs`: 工具统一执行入口（仅集成内置工具；维护 shell sessions；持有 Config 并支持 `set_config` 热更新；支持 tool 输出多条消息用于 image attach；`extra_tool_definitions` 暂为空）
  - `mcp.rs`: MCP stdio hub（连接 server、列出 tools、`mcp__<server>__<tool>` 命名空间、调用工具；当前未接入 ToolEngine）
  - `skills.rs`: skills 发现（扫描 roots；按 `id` 去重；解析 `SKILL.md` YAML front matter 的 `name/description`；剥离 front matter 得到正文）

### crates/kiliax-tui

TUI 交互式对话界面（ratatui + crossterm）：inline viewport（参考 codex）+ 终端 scrollback 历史；启动先插入 header（版本/模型/cwd），输入框从 header 之后开始并随输出自动下推；输入框支持自动换行与动态高度。

- `crates/kiliax-tui/Cargo.toml`: TUI 依赖（`ratatui`/`crossterm`/`pulldown-cmark`/`syntect` 等）
- `crates/kiliax-tui/src/main.rs`: 入口与事件循环（键盘输入 + AgentRuntime 流 + message queue 自动串行发送）；slash command 分发（/agent、/model）与模型切换落盘
- `crates/kiliax-tui/src/app.rs`: `App` 状态（stream collector/不交织 thinking；turn/step/tool 计时；工具调用折叠展示；图片附件以输入框内联 token `[img#N]` 形式挂载/删除；提交时自动剥离 token 仅发送图片；message queue：运行中提交入队、Ctrl+C 撤回、↑ 回溯编辑；提供队列预览数据给 UI）；slash command（/agent、/model）与 UI mode（chat/model picker）状态机；模型切换会更新 `killiax.yaml` 的 `default_model` 并热切换 runtime（同步 reload Config + `ToolEngine::set_config`）；切换后 checkpoint session（刷新 system preamble）
- `crates/kiliax-tui/src/ui.rs`: codex 风格 composer（`›` 前缀、自动换行、动态高度；`[img#N]` token 蓝色高亮显示；输入框上方 queue 列表：tool-call 风格且高亮 queue）；slash command popup（显示在输入框下方）；model picker 选择界面；状态行/底部 footer（仅 status/快捷键，不展示 agent/model）
- `crates/kiliax-tui/src/header.rs`: 启动信息栏（版本/模型/cwd）渲染为 history lines
- `crates/kiliax-tui/src/clipboard_paste.rs`: 剪切板图片读取→临时 PNG 路径（arboard + WSL PowerShell fallback）；粘贴路径规范化（file://、Windows/UNC、WSL 映射）
- `crates/kiliax-tui/src/slash_command.rs`: slash command 定义 + popup 状态（↑/↓ 选择、Tab 补全、/a alias）
- `crates/kiliax-tui/src/model_picker.rs`: model picker 状态（provider/model 列表、模糊搜索、键盘导航；返回值总是 provider-qualified model id，兼容带 `/` 的模型名）
- `crates/kiliax-tui/src/style.rs`: composer 灰底样式与 diff 行背景（从终端默认背景色推导，类似 codex）
- `crates/kiliax-tui/src/markdown.rs`: Markdown 渲染（紧凑输出：不额外插入空行；pulldown-cmark → ratatui `Line`）；fenced code block 调用语法高亮；包含渲染紧凑性相关单元测试
- `crates/kiliax-tui/src/highlight.rs`: 代码语法高亮（syntect scope → VS Code Dark+ 默认配色 → ratatui spans）
- `crates/kiliax-tui/src/wrap.rs`: styled 文本按终端宽度换行（含宽字符/样式保持单元测试）
- `crates/kiliax-tui/src/input.rs`: 单行输入编辑（cursor/backspace/delete 等）；`[img#N]` token 原子移动/删除；支持整行替换（历史回填）；包含 Unicode/快捷键单元测试
- `crates/kiliax-tui/src/custom_terminal.rs`: TUI 用到的自定义终端命令（scroll region、wraparound 开关、RI 等 ANSI 序列封装）
- `crates/kiliax-tui/src/terminal.rs`: inline viewport backend（viewport 可下推/可伸缩；raw mode + bracketed paste）；仅重绘 viewport；跳过绘制终端右下角单元格以规避部分终端的滚屏/空行问题；每帧用 Synchronized Update 包裹所有写入（同一 backend writer）以减少 tearing
- `crates/kiliax-tui/src/history.rs`: 向 viewport 之上插入历史行（展开渲染 user bubble 与 turn divider marker；user bubble 使用与输入框一致的灰底样式并保留上下 padding；支持 fg/bg/italic 等 Style；渲染宽度预留 1 列避免 xterm.js 末列自动换行；codex 风格：用 DECSTBM 设置 scroll region，先用 RI(`ESC M`) 下推 viewport，再在上方 scroll region 底部用 CRLF(`\\r\\n`) 逐行插入；插入时临时关闭 wraparound + 过滤 `\\r/\\n`；必要时清理 continuation rows 以避免残留/空行；相关 ANSI 命令封装在 `custom_terminal`）；包含 marker 解析/user bubble/divider 展开单元测试

## constraints

### TUI

后续修改必须保持一致
- user bubble：历史区渲染需与输入框风格一致（灰底 + 上下 padding）
- thinking：以灰色斜体显示；可以流式，但**不得**与正文输出交织；正文开始后应关闭/忽略后续 thinking delta
- 输出紧凑：避免引入多余空行（尤其是 thinking/流式渲染导致的空行）

- 遵循无框线的设计，简化UI，必要时可以使用灰色背景，或者使用蓝色高亮作为区分。

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
- 优先遵循最佳实践，并兼顾常见终端兼容性（尤其 VSCode WSL / xterm.js），遗留代码可大胆重构
- 不要修改TODO.md
